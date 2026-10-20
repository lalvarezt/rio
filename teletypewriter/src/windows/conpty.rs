use crate::Winsize;
use std::io::{Error, Result};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::OnceLock;
use std::{mem, ptr};
use tracing::*;

use crate::windows::pipes::{EventedAnonRead, EventedAnonWrite};

use windows_sys::core::{HRESULT, PWSTR};
use windows_sys::Win32::Foundation::{FreeLibrary, HANDLE};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::{s, w};

use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, STARTUPINFOW,
};

use std::os::windows::ffi::OsStrExt;

use crate::windows::child::ChildExitWatcher;
use crate::windows::{cmdline, win32_string, Pty};

// Buffer a full ring-sized burst in the native pipe; the smaller system
// default adds blocking handoffs even when the ring still has room.
const PIPE_BUFFER_SIZE: u32 = 64 * 1024;

/// Load the pseudoconsole API from conpty.dll if possible, otherwise use the
/// standard Windows API.
///
/// The conpty.dll from the Windows Terminal project
/// supports loading OpenConsole.exe, which offers many improvements and
/// bugfixes compared to the standard conpty that ships with Windows.
///
/// The conpty.dll and OpenConsole.exe files will be searched in PATH and in
/// the directory where Rio's executable is located.
type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

struct ConptyApi {
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

impl ConptyApi {
    fn get() -> &'static Self {
        // Keep one DLL reference for the function pointers' process lifetime,
        // rather than loading and leaking another reference for every tab.
        static API: OnceLock<ConptyApi> = OnceLock::new();
        API.get_or_init(Self::new)
    }

    fn new() -> Self {
        match Self::load_conpty() {
            Some(conpty) => {
                info!("Using conpty.dll for pseudoconsole");
                conpty
            }
            None => {
                // Cannot load conpty.dll - use the standard Windows API.
                info!("Using Windows API for pseudoconsole");
                Self {
                    create: CreatePseudoConsole,
                    resize: ResizePseudoConsole,
                    close: ClosePseudoConsole,
                }
            }
        }
    }

    /// Try loading ConptyApi from conpty.dll library.
    fn load_conpty() -> Option<Self> {
        type LoadedFn = unsafe extern "system" fn() -> isize;
        unsafe {
            let hmodule = LoadLibraryW(w!("conpty.dll"));
            if hmodule.is_null() {
                return None;
            }
            let (Some(create_fn), Some(resize_fn), Some(close_fn)) = (
                GetProcAddress(hmodule, s!("CreatePseudoConsole")),
                GetProcAddress(hmodule, s!("ResizePseudoConsole")),
                GetProcAddress(hmodule, s!("ClosePseudoConsole")),
            ) else {
                FreeLibrary(hmodule);
                return None;
            };

            Some(Self {
                create: mem::transmute::<LoadedFn, CreatePseudoConsoleFn>(create_fn),
                resize: mem::transmute::<LoadedFn, ResizePseudoConsoleFn>(resize_fn),
                close: mem::transmute::<LoadedFn, ClosePseudoConsoleFn>(close_fn),
            })
        }
    }
}

/// RAII Pseudoconsole.
pub struct Conpty {
    pub handle: HPCON,
    api: &'static ConptyApi,
}

impl Drop for Conpty {
    fn drop(&mut self) {
        // On older Windows this can wait for output to drain. Pty closes its
        // read pipe first, so a full transport queue cannot deadlock teardown.
        unsafe { (self.api.close)(self.handle) }
    }
}

// The ConPTY handle can be sent between threads.
unsafe impl Send for Conpty {}

struct ProcessAttributes(Box<[usize]>);

impl ProcessAttributes {
    fn new() -> Result<Self> {
        let mut size = 0;
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(Error::last_os_error());
        }
        // The native structure requires pointer alignment, not byte alignment.
        let mut storage =
            vec![0; size.div_ceil(mem::size_of::<usize>())].into_boxed_slice();
        if unsafe {
            InitializeProcThreadAttributeList(
                storage.as_mut_ptr().cast(),
                1,
                0,
                &mut size,
            )
        } == 0
        {
            return Err(Error::last_os_error());
        }
        Ok(Self(storage))
    }
}

impl Drop for ProcessAttributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.0.as_mut_ptr().cast());
        }
    }
}

/// Builds a `CREATE_UNICODE_ENVIRONMENT` block from the current process
/// environment plus `extra_env` (which overrides inherited variables of the
/// same name): NUL-terminated `KEY=VALUE` UTF-16 entries, with a trailing
/// NUL. Sorted case-insensitively by name, which the CreateProcess
/// documentation requires of Unicode environment blocks.
fn environment_block(extra_env: Vec<(String, String)>) -> Vec<u16> {
    let mut vars: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        std::env::vars_os().collect();
    for (key, value) in extra_env {
        let key = std::ffi::OsString::from(key);
        vars.retain(|(existing, _)| !existing.eq_ignore_ascii_case(&key));
        vars.push((key, value.into()));
    }
    vars.sort_by(|(a, _), (b, _)| {
        a.to_string_lossy()
            .to_uppercase()
            .cmp(&b.to_string_lossy().to_uppercase())
    });

    let mut block = Vec::new();
    for (key, value) in vars {
        block.extend(key.encode_wide());
        block.push('=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

pub fn new(
    shell: Option<&str>,
    working_directory: &Option<String>,
    env: Option<Vec<(String, String)>>,
    columns: u16,
    rows: u16,
) -> Result<Pty> {
    let api = ConptyApi::get();
    let mut pty_handle: HPCON = 0;

    let (conout, conout_pty_handle) = miow::pipe::anonymous(PIPE_BUFFER_SIZE)?;
    let (conin_pty_handle, conin) = miow::pipe::anonymous(PIPE_BUFFER_SIZE)?;

    let winsize = Winsize {
        ws_row: rows as libc::c_ushort,
        ws_col: columns as libc::c_ushort,
        ws_xpixel: 0 as libc::c_ushort,
        ws_ypixel: 0 as libc::c_ushort,
    };

    // Create the Pseudo Console, using the pipes.
    let result = unsafe {
        (api.create)(
            winsize.into(),
            conin_pty_handle.as_raw_handle(),
            conout_pty_handle.as_raw_handle(),
            0,
            &mut pty_handle as *mut _,
        )
    };

    if result < 0 {
        return Err(Error::other(format!(
            "CreatePseudoConsole failed (HRESULT {result:#010x})"
        )));
    }
    let conpty = Conpty {
        handle: pty_handle,
        api,
    };
    // The pseudoconsole has duplicated these endpoints. Keeping ours open
    // prevents EOF detection and leaks two handles per terminal.
    drop(conin_pty_handle);
    drop(conout_pty_handle);

    let mut success;

    // Prepare child process startup info.

    let mut startup_info_ex: STARTUPINFOEXW = unsafe { mem::zeroed() };

    startup_info_ex.StartupInfo.lpTitle = std::ptr::null_mut() as PWSTR;

    startup_info_ex.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;

    // Setting this flag but leaving all the handles as default (null) ensures the
    // PTY process does not inherit any handles from this Rio process.
    startup_info_ex.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;

    let mut attr_list = ProcessAttributes::new()?;
    startup_info_ex.lpAttributeList = attr_list.0.as_mut_ptr().cast();

    // Set thread attribute list's Pseudo Console to the specified ConPTY.
    unsafe {
        success = UpdateProcThreadAttribute(
            startup_info_ex.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pty_handle as *mut std::ffi::c_void,
            mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null_mut(),
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    let mut cmdline = win32_string(&cmdline(shell));
    let cwd = working_directory.as_ref().map(win32_string);
    // With no overrides, leave the environment pointer null so the child
    // inherits ours, exactly as before.
    let mut env_block = env.map(environment_block);

    let mut proc_info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    unsafe {
        success = CreateProcessW(
            ptr::null(),
            cmdline.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            false as i32,
            match env_block {
                Some(_) => EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                None => EXTENDED_STARTUPINFO_PRESENT,
            },
            match env_block.as_mut() {
                Some(block) => block.as_mut_ptr() as *mut std::ffi::c_void,
                None => ptr::null_mut(),
            },
            cwd.as_ref().map_or_else(ptr::null, |s| s.as_ptr()),
            &mut startup_info_ex.StartupInfo as *mut STARTUPINFOW,
            &mut proc_info as *mut PROCESS_INFORMATION,
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    let process = unsafe { OwnedHandle::from_raw_handle(proc_info.hProcess) };
    // Only the process wait is needed; the primary thread handle has no owner
    // in the raw PROCESS_INFORMATION returned by CreateProcessW.
    drop(unsafe { OwnedHandle::from_raw_handle(proc_info.hThread) });
    let child_watcher = match ChildExitWatcher::new(process) {
        Ok(watcher) => watcher,
        Err(error) => {
            // No worker exists to drain output on this failure path.
            drop(conout);
            drop(conin);
            return Err(error);
        }
    };
    let conin = EventedAnonWrite::new(conin);
    let conout = EventedAnonRead::new(conout);

    Ok(Pty::new(conpty, conout, conin, child_watcher))
}

impl Conpty {
    pub fn on_resize(&mut self, window_size: Winsize) -> Result<()> {
        let result = unsafe { (self.api.resize)(self.handle, window_size.into()) };
        if result < 0 {
            Err(Error::other(format!(
                "ResizePseudoConsole failed (HRESULT {result:#010x})"
            )))
        } else {
            Ok(())
        }
    }
}

impl From<Winsize> for COORD {
    fn from(window_size: Winsize) -> Self {
        let lines = window_size.ws_row;
        let columns = window_size.ws_col;
        COORD {
            X: columns as i16,
            Y: lines as i16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessHandleCount,
    };

    fn handles() -> u32 {
        let mut count = 0;
        assert_ne!(
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
            0
        );
        count
    }

    #[test]
    fn failed_spawn_and_loaded_pty_teardown_release_handles() {
        // Warm lazy DLL/runtime initialization before comparing process handles.
        drop(new(Some("cmd.exe /d /c exit"), &None, None, 80, 24).unwrap());
        let before = handles();
        for _ in 0..20 {
            assert!(new(Some("rio-nonexistent-shell.exe"), &None, None, 80, 24).is_err());
            let mut pty = new(
                Some("cmd.exe /d /c for /L %i in (1,1,1000000) do @echo transport-test"),
                &None,
                None,
                80,
                24,
            )
            .unwrap();
            for (columns, rows) in [(100, 30), (40, 10), (120, 40)] {
                pty.backend
                    .on_resize(Winsize {
                        ws_col: columns,
                        ws_row: rows,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    })
                    .unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            drop(pty);
        }
        assert!(
            handles() <= before + 2,
            "PTY creation/destruction leaked process handles"
        );
    }

    #[test]
    fn failed_resize_returns_an_error_instead_of_panicking() {
        // Some ConPTY versions accept a zero size. An invalid handle exercises
        // failure consistently, without depending on size-clamping policy.
        let mut conpty = mem::ManuallyDrop::new(Conpty {
            handle: 0,
            api: ConptyApi::get(),
        });
        assert!(conpty
            .on_resize(Winsize {
                ws_col: 80,
                ws_row: 24,
                ws_xpixel: 0,
                ws_ypixel: 0
            })
            .is_err());
    }
}
