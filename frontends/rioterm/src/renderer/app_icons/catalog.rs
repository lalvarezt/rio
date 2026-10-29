use super::{logos, Icon, DEFAULT_ICON};
use rustc_hash::FxHashMap;
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    #[default]
    App,
    Shell,
    Runtime,
    Transport,
}

#[derive(Clone, Copy)]
struct App {
    icon: Icon,
    kind: Kind,
    versioned: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    icon: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    kind: Kind,
    #[serde(default)]
    versioned: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definitions {
    icons: BTreeMap<String, char>,
    apps: Vec<Definition>,
}

struct Catalog {
    apps: FxHashMap<String, App>,
    icons: FxHashMap<String, Icon>,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let definitions: Definitions =
        toml::from_str(include_str!("catalog.toml")).expect("valid bundled app catalog");
    let mut icons: FxHashMap<_, _> = definitions
        .icons
        .into_iter()
        .map(|(name, glyph)| (name, Icon::Glyph(glyph)))
        .collect();
    for logo in logos::LOGOS {
        icons.insert(logo.name.into(), Icon::Logo(logo));
    }
    let mut apps = FxHashMap::default();
    for definition in definitions.apps {
        let app = App {
            icon: *icons.get(&definition.icon).expect("known catalog icon"),
            kind: definition.kind,
            versioned: definition.versioned,
        };
        for name in std::iter::once(definition.id).chain(definition.aliases) {
            assert!(
                apps.insert(normalize(&name).into_owned(), app).is_none(),
                "duplicate app alias: {name}"
            );
        }
    }
    Catalog { apps, icons }
});

fn normalize(program: &str) -> Cow<'_, str> {
    let name = program
        .trim()
        .trim_matches(['\'', '"'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let name = [".exe", ".cmd", ".bat", ".ps1", ".com"]
        .into_iter()
        .find_map(|extension| {
            let end = name.len().checked_sub(extension.len())?;
            name.get(end..)?
                .eq_ignore_ascii_case(extension)
                .then(|| &name[..end])
        })
        .unwrap_or(name);
    if name.bytes().any(|c| c.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

#[derive(Default)]
pub struct Library {
    overrides: FxHashMap<String, App>,
}

impl Library {
    pub fn configure(&mut self, mappings: &BTreeMap<String, String>) {
        self.overrides.clear();
        for (command, target) in mappings {
            let target = normalize(target);
            let app = CATALOG
                .apps
                .get(target.as_ref())
                .map(|app| app.icon)
                .or_else(|| CATALOG.icons.get(target.as_ref()).copied())
                .map(|icon| App {
                    icon,
                    kind: Kind::App,
                    versioned: false,
                });
            match app {
                Some(app) => {
                    self.overrides.insert(normalize(command).into_owned(), app);
                }
                None => {
                    tracing::warn!("Unknown tab icon mapping for {command}: {target}")
                }
            }
        }
    }

    fn lookup(&self, program: &str) -> Option<App> {
        self.lookup_name(normalize(program).as_ref())
    }

    fn lookup_name(&self, name: &str) -> Option<App> {
        self.overrides
            .get(name)
            .or_else(|| CATALOG.apps.get(name))
            .copied()
            .or_else(|| {
                let base =
                    name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
                CATALOG
                    .apps
                    .get(base)
                    .copied()
                    .filter(|app| app.versioned && base != name)
            })
    }

    fn title_hint(&self, title: &str) -> Option<Icon> {
        let icon_for = |candidate: &str| {
            self.lookup(candidate.trim_matches(['[', ']', ':']))
                .filter(|app| matches!(app.kind, Kind::App | Kind::Runtime))
                .map(|app| app.icon)
        };
        for segment in title.split(" - ") {
            let segment = segment.trim().trim_matches(['[', ']', '*']);
            // Paths and project names are not evidence of a running app.
            if segment.contains(['/', '\\'])
                && !segment.to_ascii_lowercase().ends_with(".exe")
            {
                continue;
            }
            if let Some(icon) = icon_for(segment) {
                return Some(icon);
            }
            let mut words = segment.split_whitespace();
            let Some(first) = words.next() else { continue };
            if let Some(second) = words.next() {
                if let Some(icon) = icon_for(&format!("{first} {second}")) {
                    return Some(icon);
                }
            }
            if let Some(icon) = icon_for(first) {
                return Some(icon);
            }
        }
        None
    }

    pub fn resolve(
        &self,
        program: &str,
        title: &str,
        status: Option<&rio_backend::program_status::ProgramStatusRecord>,
        reported: bool,
    ) -> (Icon, bool) {
        use rio_backend::program_status::ProgramState;
        let name = normalize(program);
        // Transport identity belongs to the command, independently of any
        // user-selected icon. Keep badges and aliases in the same catalog.
        let remote = CATALOG
            .apps
            .get(name.as_ref())
            .is_some_and(|app| app.kind == Kind::Transport);
        if let Some(record) = status {
            if matches!(record.state, ProgramState::Working | ProgramState::Blocked) {
                if let Some(app) = record.app.as_deref().and_then(|app| self.lookup(app))
                {
                    return (app.icon, remote);
                }
            }
        }
        let app = self.lookup_name(&name);
        // App reports are authoritative, including at a shell prompt. Do not
        // resurrect a previous app from a stale title or duplicate the SSH badge.
        if reported {
            let icon = app
                .filter(|app| app.kind != Kind::Transport)
                .map_or(DEFAULT_ICON, |app| app.icon);
            return (icon, remote);
        }
        if let Some(icon) = self.title_hint(title) {
            return (icon, remote);
        }
        let icon = app
            .filter(|app| app.kind != Kind::Transport)
            .map_or(DEFAULT_ICON, |app| app.icon);
        (icon, remote)
    }
}
