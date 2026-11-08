//! Templates (docs/11-composition.md §7): a profile file with holes (`{{name}}`) and a
//! `[template]` table that names it, declares its holes and the modules it needs.
//!
//! Applying one is literal substitution over the text, nothing more: every `{{name}}` whose
//! name is a declared var becomes the var's value, verbatim, in one pass (a value is never
//! scanned again). There is no escape syntax and no expressions. The result, without the
//! `[template]` table, is parsed as a profile, checked, and written as a copy the project
//! owns. Templates are files, read only when a client asks: the core keeps none in memory.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use regulus_proto::{Profile, TemplateRequire, TemplateSummary, TemplateVar};
use serde::Deserialize;

use crate::registry::base_id;

/// The table that turns a profile file into a template. It is stripped before rendering.
const TABLE: &str = "template";
/// The extension of template files, and of the profile files they become.
const EXTENSION: &str = "toml";
/// Holes open and close with these.
const OPEN: &str = "{{";
const CLOSE: &str = "}}";

/// Where templates come from and where applied ones go.
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// Files or directories with templates (`[templates] paths`), already resolved.
    pub templates: Vec<PathBuf>,
    /// Where an applied template is written: the first `[profiles] paths` entry. `None`
    /// refuses to apply.
    pub profiles_dir: Option<PathBuf>,
    /// What a relative template path given by a client is resolved against.
    pub home: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Params(String),
    #[error("{0}")]
    Unmet(String),
    #[error("{0}")]
    Exists(String),
    #[error("{0}")]
    Io(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    requires: Vec<Require>,
    #[serde(default)]
    vars: BTreeMap<String, VarSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Require {
    module: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VarSpec {
    #[serde(default)]
    description: String,
    #[serde(default)]
    default: Option<toml::Value>,
}

/// A template read from disk.
#[derive(Debug, Clone)]
pub struct Template {
    pub path: PathBuf,
    /// The file as it is.
    pub text: String,
    pub name: String,
    pub description: String,
    pub requires: Vec<TemplateRequire>,
    pub vars: Vec<TemplateVar>,
    /// The text without the `[template]` table: what gets rendered.
    body: String,
}

impl Template {
    pub fn summary(&self) -> TemplateSummary {
        TemplateSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            path: self.path.display().to_string(),
            vars: self.vars.clone(),
            requires: self.requires.clone(),
            error: None,
        }
    }
}

/// A var name: what may stand between the braces of a hole.
fn is_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The key of a table header line (`[a.b]` or `[[a.b]]`), if the line is one.
fn header_key(line: &str) -> Option<&str> {
    let line = line.trim();
    let line = line
        .split_once('#')
        .map_or(line, |(code, _)| code)
        .trim_end();
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner
        .strip_prefix('[')
        .and_then(|i| i.strip_suffix(']'))
        .unwrap_or(inner)
        .trim();
    let plain = !inner.is_empty() && !inner.contains(['=', ',', '{', '[', ']']);
    plain.then_some(inner)
}

fn is_template_key(key: &str) -> bool {
    let key = key.trim_matches('"');
    key == TABLE || key.starts_with(&format!("{TABLE}."))
}

/// Splits a template into its `[template]` table (and subtables) and the rest.
fn split(text: &str) -> (String, String) {
    let (mut meta, mut body) = (String::new(), String::new());
    let mut in_template = false;
    for line in text.split_inclusive('\n') {
        if let Some(key) = header_key(line) {
            in_template = is_template_key(key);
        }
        if in_template {
            meta.push_str(line);
        } else {
            body.push_str(line);
        }
    }
    (meta, body)
}

/// The holes of `text`: byte range and var name, in order. `{{…}}` with anything but a
/// var name inside (`{{.}}`, `{{json.q}}`) is not a hole and stays as it is.
fn holes(text: &str) -> Vec<(usize, usize, &str)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find(OPEN).map(|i| at + i) {
        let start = open + OPEN.len();
        let Some(close) = text[start..].find(CLOSE).map(|i| start + i) else {
            break;
        };
        let name = &text[start..close];
        if is_var_name(name) {
            found.push((open, close + CLOSE.len(), name));
            at = close + CLOSE.len();
        } else {
            at = start;
        }
    }
    found
}

fn scalar(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(text) => Some(text.clone()),
        toml::Value::Integer(n) => Some(n.to_string()),
        toml::Value::Float(n) => Some(n.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Reads `text` (from `path`) as a template.
pub fn parse(path: &Path, text: &str) -> Result<Template, TemplateError> {
    let invalid = |why: String| TemplateError::Invalid(format!("{}: {why}", path.display()));
    let (meta_text, body) = split(text);
    let mut table: toml::Table =
        toml::from_str(&meta_text).map_err(|e| invalid(format!("invalid [{TABLE}] table: {e}")))?;
    let meta: Meta = table
        .remove(TABLE)
        .ok_or_else(|| invalid(format!("no [{TABLE}] table")))?
        .try_into()
        .map_err(|e| invalid(format!("invalid [{TABLE}] table: {e}")))?;
    if meta.name.is_empty() || meta.name.contains('/') || meta.name.ends_with(EXTENSION) {
        return Err(invalid(format!(
            "template name `{}` must be non-empty, without `/` and not end in `.{EXTENSION}`",
            meta.name
        )));
    }
    let mut requires = Vec::with_capacity(meta.requires.len());
    for require in meta.requires {
        if require.module.is_empty() {
            return Err(invalid("a `requires` entry has an empty module".into()));
        }
        semver::VersionReq::parse(&require.version).map_err(|e| {
            invalid(format!(
                "module `{}` requires version `{}`, which is not a semver requirement: {e}",
                require.module, require.version
            ))
        })?;
        requires.push(TemplateRequire {
            module: require.module,
            version: require.version,
        });
    }
    let mut vars = Vec::with_capacity(meta.vars.len());
    for (name, spec) in meta.vars {
        if !is_var_name(&name) {
            return Err(invalid(format!(
                "var `{name}` is not a valid name (a letter or `_`, then letters, digits, `_` or `-`)"
            )));
        }
        let default = match &spec.default {
            None => None,
            Some(value) => Some(scalar(value).ok_or_else(|| {
                invalid(format!(
                    "the default of var `{name}` must be text, a number or a boolean"
                ))
            })?),
        };
        vars.push(TemplateVar {
            name,
            description: spec.description,
            default,
        });
    }
    Ok(Template {
        path: path.to_path_buf(),
        text: text.to_string(),
        name: meta.name,
        description: meta.description,
        requires,
        vars,
        body,
    })
}

/// Reads the template file at `path`.
pub fn read(path: &Path) -> Result<Template, TemplateError> {
    let text = fs::read_to_string(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => {
            TemplateError::NotFound(format!("no template at {}", path.display()))
        }
        _ => TemplateError::Io(format!("cannot read {}: {e}", path.display())),
    })?;
    parse(path, &text)
}

/// The template files under `paths`, in order: a file as is, a directory's `.toml` files
/// sorted by name. A path that does not exist holds none.
fn files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in paths {
        if path.is_file() {
            found.push(path.clone());
            continue;
        }
        let Ok(entries) = fs::read_dir(path) else {
            continue;
        };
        let mut here: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == EXTENSION))
            .collect();
        here.sort();
        found.extend(here);
    }
    found
}

/// Every template under `places`. One that cannot be read is listed with its error.
pub fn list(places: &Places) -> Vec<TemplateSummary> {
    files(&places.templates)
        .into_iter()
        .map(|path| match read(&path) {
            Ok(template) => template.summary(),
            Err(error) => TemplateSummary {
                name: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                description: String::new(),
                path: path.display().to_string(),
                vars: Vec::new(),
                requires: Vec::new(),
                error: Some(error.to_string()),
            },
        })
        .collect()
}

/// Whether `wanted` names a file rather than a template.
fn is_path(wanted: &str) -> bool {
    wanted.contains('/') || wanted.ends_with(&format!(".{EXTENSION}"))
}

/// A template by name (the first under `places` with it) or by path (anything with `/`
/// or ending in `.toml`; relative to the home directory).
pub fn find(places: &Places, wanted: &str) -> Result<Template, TemplateError> {
    if is_path(wanted) {
        let path = Path::new(wanted);
        let path = match &places.home {
            Some(home) if path.is_relative() => home.join(path),
            _ => path.to_path_buf(),
        };
        return read(&path);
    }
    files(&places.templates)
        .iter()
        .filter_map(|path| read(path).ok())
        .find(|template| template.name == wanted)
        .ok_or_else(|| {
            let paths: Vec<String> = places
                .templates
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            TemplateError::NotFound(format!(
                "no template named `{wanted}` in {}",
                paths.join(", ")
            ))
        })
}

/// Fills the holes of the template's body with `values`, falling back on each var's
/// default. A value for a var the template does not declare, a hole it does not declare,
/// or a declared var left without a value is an error that names them.
pub fn render(
    template: &Template,
    values: &BTreeMap<String, String>,
) -> Result<String, TemplateError> {
    let declared = |name: &str| template.vars.iter().any(|v| v.name == name);
    let unknown: Vec<&str> = values
        .keys()
        .map(String::as_str)
        .filter(|name| !declared(name))
        .collect();
    if !unknown.is_empty() {
        return Err(TemplateError::Params(format!(
            "template `{}` declares no var {}",
            template.name,
            quoted(&unknown)
        )));
    }
    let holes = holes(&template.body);
    let mut undeclared: Vec<&str> = Vec::new();
    for (_, _, name) in &holes {
        if !declared(name) && !undeclared.contains(name) {
            undeclared.push(name);
        }
    }
    if !undeclared.is_empty() {
        return Err(TemplateError::Invalid(format!(
            "template `{}` has holes not declared in [{TABLE}.vars]: {}",
            template.name,
            undeclared
                .iter()
                .map(|name| format!("{OPEN}{name}{CLOSE}"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let mut resolved = BTreeMap::new();
    let mut missing = Vec::new();
    for var in &template.vars {
        match values.get(&var.name).or(var.default.as_ref()) {
            Some(value) => {
                resolved.insert(var.name.as_str(), value.as_str());
            }
            None => missing.push(var.name.as_str()),
        }
    }
    if !missing.is_empty() {
        return Err(TemplateError::Params(format!(
            "template `{}` needs a value for var {}",
            template.name,
            quoted(&missing)
        )));
    }
    let mut out = String::with_capacity(template.body.len());
    let mut at = 0;
    for (start, end, name) in holes {
        out.push_str(&template.body[at..start]);
        out.push_str(resolved[name]);
        at = end;
    }
    out.push_str(&template.body[at..]);
    Ok(out)
}

fn quoted(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reads rendered text as the one profile it must hold.
pub fn profile_of(template: &Template, text: &str) -> Result<Profile, TemplateError> {
    toml::from_str(text).map_err(|e| {
        TemplateError::Invalid(format!(
            "template `{}` does not render to a profile: {e}",
            template.name
        ))
    })
}

/// Gives the rendered profile another id by rewriting its top-level `id = …` line, the one
/// before any table header. Comments and layout stay.
pub fn rename(text: &str, id: &str) -> Result<String, TemplateError> {
    let mut out = String::with_capacity(text.len() + id.len());
    let mut done = false;
    let mut in_tables = false;
    for line in text.split_inclusive('\n') {
        in_tables |= header_key(line).is_some();
        let key = line.split_once('=').map(|(key, _)| key.trim());
        if !done && !in_tables && matches!(key, Some("id" | "\"id\"")) {
            let end = if line.ends_with('\n') { "\n" } else { "" };
            out.push_str(&format!("id = {}{end}", toml::Value::String(id.into())));
            done = true;
        } else {
            out.push_str(line);
        }
    }
    if done {
        Ok(out)
    } else {
        Err(TemplateError::Invalid(
            "cannot find the profile's top-level `id = …` line to change the id".into(),
        ))
    }
}

/// The file name a profile id is written as, if the id is a single safe name: one plain
/// path component, not hidden, without separators or control characters.
pub fn file_name(id: &str) -> Result<String, TemplateError> {
    let mut components = Path::new(id).components();
    let plain = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    if !plain || id.starts_with('.') || id.contains(['/', '\\']) || id.chars().any(char::is_control)
    {
        return Err(TemplateError::Params(format!(
            "profile id `{id}` cannot be a file name: use one plain name, without `/`, `\\`, \
             a leading `.` or control characters"
        )));
    }
    Ok(format!("{id}.{EXTENSION}"))
}

/// Checks each requirement against the ids of the installed modules (`author/module@x.y.z`),
/// by id and version only: a module that is not installed, or installed only at versions the
/// requirement does not accept, is an error that names it.
pub fn check_requires<'a>(
    requires: &[TemplateRequire],
    installed: impl Iterator<Item = &'a str> + Clone,
) -> Result<(), TemplateError> {
    for require in requires {
        let wanted = base_id(&require.module);
        let versions: Vec<&str> = installed
            .clone()
            .filter(|id| base_id(id) == wanted)
            .map(|id| id.split_once('@').map_or("", |(_, version)| version))
            .collect();
        if versions.is_empty() {
            return Err(TemplateError::Unmet(format!(
                "module `{wanted}` is not installed; the template requires it ({})",
                require.version
            )));
        }
        let requirement = semver::VersionReq::parse(&require.version)
            .map_err(|e| TemplateError::Invalid(format!("`{}`: {e}", require.version)))?;
        let fits = versions.iter().any(|version| {
            semver::Version::parse(version).is_ok_and(|version| requirement.matches(&version))
        });
        if !fits {
            return Err(TemplateError::Unmet(format!(
                "module `{wanted}` is installed at {}; the template requires {}",
                versions
                    .iter()
                    .map(|v| if v.is_empty() { "no version" } else { v })
                    .collect::<Vec<_>>()
                    .join(", "),
                require.version
            )));
        }
    }
    Ok(())
}

/// Writes `text` as `<dir>/<id>.toml` through a temporary file renamed over it, so a
/// reader never sees half of it and a symlink there is replaced, not followed.
pub fn write(dir: &Path, id: &str, text: &str) -> Result<PathBuf, TemplateError> {
    let name = file_name(id)?;
    let io = |what: &str, path: &Path, e: io::Error| {
        TemplateError::Io(format!("cannot {what} {}: {e}", path.display()))
    };
    if dir.is_file() {
        return Err(TemplateError::Io(format!(
            "{} is a file; the first [profiles] paths entry must be a directory to write to",
            dir.display()
        )));
    }
    fs::create_dir_all(dir).map_err(|e| io("create", dir, e))?;
    let dest = dir.join(&name);
    if dest.parent() != Some(dir) {
        return Err(TemplateError::Params(format!(
            "profile id `{id}` would leave {}",
            dir.display()
        )));
    }
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    fs::write(&temp, text).map_err(|e| io("write", &temp, e))?;
    fs::rename(&temp, &dest).map_err(|e| {
        let _ = fs::remove_file(&temp);
        io("write", &dest, e)
    })?;
    Ok(dest)
}

/// The first lines of an applied copy: where it came from.
pub fn provenance(template: &Template) -> String {
    format!(
        "# Applied from template `{}` ({}). This copy belongs to the project: edit it freely.\n",
        template.name,
        template.path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECHO: &str = r#"# A test template.
id = "echo"
name = "Echo {{who}}"
main_workflow_id = "main"

[template]
name = "echo"
description = "Says hello"
requires = [{ module = "regulus/hello", version = ">=0.1" }]

[template.vars]
who = { description = "who is greeted" }
greeting = { description = "the word", default = "Hello" }
input = { description = "kept for the module", default = "{{input}}" }

[[workflows]]
id = "main"
kind = "main"

  [[workflows.nodes]]
  id = "ana"
  type = "greeter"
  module_id = "regulus/hello@0.1.0"
  config = { name = "{{greeting}}, {{who}}", args = ["{{input}}", "{{.}}", "{{ json.q }}"] }
"#;

    fn template() -> Template {
        parse(Path::new("/t/echo.toml"), ECHO).unwrap()
    }

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_template_table_is_read_and_stripped() {
        let t = template();
        assert_eq!(t.name, "echo");
        assert_eq!(t.description, "Says hello");
        assert_eq!(t.requires[0].module, "regulus/hello");
        let names: Vec<&str> = t.vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["greeting", "input", "who"]);
        let text = render(&t, &values(&[("who", "Ana")])).unwrap();
        assert!(!text.contains("[template"), "{text}");
        assert!(!text.contains("Says hello"), "{text}");
        let profile = profile_of(&t, &text).unwrap();
        assert_eq!(profile.id, "echo");
        assert_eq!(profile.name, "Echo Ana");
        assert_eq!(profile.workflows[0].nodes[0].id, "ana");
    }

    #[test]
    fn a_default_fills_a_hole_and_other_braces_stay() {
        let text = render(&template(), &values(&[("who", "Ana")])).unwrap();
        assert!(text.contains(r#"name = "Hello, Ana""#), "{text}");
        // A default may hold a module's own placeholder: values are not scanned again.
        assert!(
            text.contains(r#"["{{input}}", "{{.}}", "{{ json.q }}"]"#),
            "{text}"
        );
        let text = render(&template(), &values(&[("who", "Bo"), ("greeting", "Hi")])).unwrap();
        assert!(text.contains(r#"name = "Hi, Bo""#), "{text}");
    }

    #[test]
    fn a_var_without_value_is_named() {
        let error = render(&template(), &BTreeMap::new()).unwrap_err();
        assert!(
            matches!(&error, TemplateError::Params(m) if m.contains("`who`")),
            "{error:?}"
        );
    }

    #[test]
    fn a_value_for_an_undeclared_var_is_named() {
        let error = render(&template(), &values(&[("who", "Ana"), ("whom", "x")])).unwrap_err();
        assert!(
            matches!(&error, TemplateError::Params(m) if m.contains("`whom`")),
            "{error:?}"
        );
    }

    #[test]
    fn an_undeclared_hole_is_named() {
        let text = ECHO.replace("Echo {{who}}", "Echo {{who}} {{model}}");
        let t = parse(Path::new("/t/echo.toml"), &text).unwrap();
        let error = render(&t, &values(&[("who", "Ana")])).unwrap_err();
        assert!(
            matches!(&error, TemplateError::Invalid(m) if m.contains("{{model}}")),
            "{error:?}"
        );
    }

    #[test]
    fn a_file_without_the_table_or_with_a_bad_requirement_is_invalid() {
        let error = parse(Path::new("p.toml"), "id = \"x\"\n").unwrap_err();
        assert!(matches!(&error, TemplateError::Invalid(m) if m.contains("no [template]")));
        let text = ECHO.replace(">=0.1", "newest");
        let error = parse(Path::new("p.toml"), &text).unwrap_err();
        assert!(matches!(&error, TemplateError::Invalid(m) if m.contains("newest")));
    }

    #[test]
    fn requires_are_checked_by_id_and_version() {
        let need = |module: &str, version: &str| {
            vec![TemplateRequire {
                module: module.into(),
                version: version.into(),
            }]
        };
        let installed = ["regulus/hello@0.1.0", "regulus/flow@0.2.3"];
        let ids = || installed.iter().copied();
        assert_eq!(
            check_requires(&need("regulus/hello", ">=0.1"), ids()),
            Ok(())
        );
        assert_eq!(check_requires(&need("regulus/flow", "^0.2"), ids()), Ok(()));
        let missing = check_requires(&need("regulus/files", ">=0.1"), ids()).unwrap_err();
        assert!(
            matches!(&missing, TemplateError::Unmet(m) if m.contains("`regulus/files` is not installed")),
            "{missing:?}"
        );
        let old = check_requires(&need("regulus/hello", ">=0.2"), ids()).unwrap_err();
        assert!(
            matches!(&old, TemplateError::Unmet(m) if m.contains("0.1.0") && m.contains(">=0.2")),
            "{old:?}"
        );
    }

    #[test]
    fn only_a_single_safe_name_becomes_a_file() {
        assert_eq!(file_name("echo-2").unwrap(), "echo-2.toml");
        for bad in [
            "", ".", "..", "../x", "a/b", "/abs", ".hidden", "a\\b", "a\nb",
        ] {
            assert!(file_name(bad).is_err(), "{bad:?} was accepted");
        }
        let dir = std::env::temp_dir().join(format!("regulus-template-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(write(&dir, "../escape", "x").is_err());
        assert!(!dir.join("../escape.toml").exists());
        let path = write(&dir, "ok", "id = \"ok\"\n").unwrap();
        assert_eq!(path, dir.join("ok.toml"));
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "the temporary file is gone"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_changes_only_the_top_level_id() {
        let text = render(&template(), &values(&[("who", "Ana")])).unwrap();
        let renamed = rename(&text, "mine").unwrap();
        let profile = profile_of(&template(), &renamed).unwrap();
        assert_eq!(profile.id, "mine");
        assert_eq!(profile.workflows[0].id, "main");
        assert_eq!(profile.workflows[0].nodes[0].id, "ana");
        assert!(renamed.starts_with("# A test template."));
    }
}
