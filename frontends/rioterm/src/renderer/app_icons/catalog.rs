use super::{logos, Icon, DEFAULT_ICON};
use rustc_hash::FxHashMap;
use serde::Deserialize;
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
    icons.insert("neovim".into(), Icon::Neovim);
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
                apps.insert(normalize(&name), app).is_none(),
                "duplicate app alias: {name}"
            );
        }
    }
    Catalog { apps, icons }
});

fn normalize(program: &str) -> String {
    let name = program
        .trim()
        .trim_matches(['\'', '"'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    for extension in [".exe", ".cmd", ".bat", ".ps1", ".com"] {
        if let Some(name) = name.strip_suffix(extension) {
            return name.to_owned();
        }
    }
    name
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
                .get(&target)
                .map(|app| app.icon)
                .or_else(|| CATALOG.icons.get(&target).copied())
                .map(|icon| App {
                    icon,
                    kind: Kind::App,
                    versioned: false,
                });
            match app {
                Some(app) => {
                    self.overrides.insert(normalize(command), app);
                }
                None => {
                    tracing::warn!("Unknown tab icon mapping for {command}: {target}")
                }
            }
        }
    }

    fn lookup(&self, program: &str) -> Option<App> {
        let name = normalize(program);
        self.overrides
            .get(&name)
            .or_else(|| CATALOG.apps.get(&name))
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
        for segment in title.split(" - ") {
            let segment = segment.trim().trim_matches(['[', ']', '*']);
            // Paths and project names are not evidence of a running app.
            if segment.contains(['/', '\\'])
                && !segment.to_ascii_lowercase().ends_with(".exe")
            {
                continue;
            }
            let words: Vec<_> = segment.split_whitespace().take(2).collect();
            let candidates = [
                Some(segment.to_owned()),
                (!words.is_empty()).then(|| words.join(" ")),
                words.first().map(|word| word.to_string()),
            ];
            for candidate in candidates.into_iter().flatten() {
                if let Some(app) = self.lookup(candidate.trim_matches(['[', ']', ':'])) {
                    if matches!(app.kind, Kind::App | Kind::Runtime) {
                        return Some(app.icon);
                    }
                }
            }
        }
        None
    }

    pub fn for_program(
        &self,
        program: &str,
        title: &str,
        status: Option<&rio_backend::program_status::ProgramStatusRecord>,
        reported: bool,
    ) -> Icon {
        use rio_backend::program_status::ProgramState;
        if let Some(record) = status {
            if matches!(record.state, ProgramState::Working | ProgramState::Blocked) {
                if let Some(app) = record.app.as_deref().and_then(|app| self.lookup(app))
                {
                    return app.icon;
                }
            }
        }
        let app = self.lookup(program);
        // App reports are authoritative, including at a shell prompt. Do not
        // resurrect a previous app from a stale title or duplicate the SSH badge.
        if reported {
            return app
                .filter(|app| app.kind != Kind::Transport)
                .map_or(DEFAULT_ICON, |app| app.icon);
        }
        if let Some(icon) = self.title_hint(title) {
            return icon;
        }
        app.filter(|app| app.kind != Kind::Transport)
            .map_or(DEFAULT_ICON, |app| app.icon)
    }
}
