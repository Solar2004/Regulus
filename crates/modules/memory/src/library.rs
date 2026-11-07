//! A skill library that grows: a value describing a skill becomes `<base>/<name>/SKILL.md`,
//! the folder layout the Skills module's `skill` node reads (`regulus_contract_skill`).

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use regulus_contract_skill::ENTRY;
use serde_json::Value;

/// The longest skill name and description agents' CLIs accept.
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 1024;
/// Where the frontmatter of a SKILL.md opens and closes.
const FENCE: &str = "---";

/// A skill ready to be written: its folder name and the whole SKILL.md.
#[derive(Debug, PartialEq)]
pub(crate) struct Skill {
    pub name: String,
    pub text: String,
}

fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{name}` is not a skill name: lowercase letters, digits and `-`, at most \
             {MAX_NAME}, not starting with `-`"
        ))
    }
}

fn check_description(description: &str) -> Result<String, String> {
    let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if description.is_empty() {
        return Err("a skill needs a description: agents pick skills by it".into());
    }
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(format!(
            "the description is longer than {MAX_DESCRIPTION} characters"
        ));
    }
    Ok(description)
}

/// A frontmatter value, without the quotes YAML may put around it.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        if let Ok(Value::String(s)) = serde_json::from_str(value) {
            return s;
        }
    }
    value.trim_matches('\'').to_string()
}

/// The skill a value describes: `{ name, description, body }`, or a SKILL.md text (a
/// string, or a JSON string that holds one) with `name` and `description` in its
/// frontmatter.
pub(crate) fn parse(value: &Value) -> Result<Skill, String> {
    let value = match value {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(object @ Value::Object(_)) => object,
            _ => return from_text(text),
        },
        other => other.clone(),
    };
    let field = |name: &str| value.get(name).and_then(Value::as_str).unwrap_or_default();
    let name = field("name").trim().to_string();
    check_name(&name)?;
    let description = check_description(field("description"))?;
    let body = field("body").trim();
    if body.is_empty() {
        return Err(format!("skill `{name}` has no `body`"));
    }
    // A JSON string is a valid YAML scalar: the description cannot break the frontmatter.
    let quoted = Value::String(description).to_string();
    Ok(Skill {
        text: format!("{FENCE}\nname: {name}\ndescription: {quoted}\n{FENCE}\n\n{body}\n"),
        name,
    })
}

fn from_text(text: &str) -> Result<Skill, String> {
    let missing = || {
        "a text skill must be a SKILL.md: frontmatter between `---` lines with `name` and \
         `description`, then the body; or send JSON { name, description, body }"
            .to_string()
    };
    let rest = text.trim_start().strip_prefix(FENCE).ok_or_else(missing)?;
    let end = rest.find(&format!("\n{FENCE}")).ok_or_else(missing)?;
    let (mut name, mut description) = (None, None);
    for line in rest[..end].lines() {
        if let Some((field, value)) = line.split_once(':') {
            match field.trim() {
                "name" => name = Some(unquote(value)),
                "description" => description = Some(unquote(value)),
                _ => {}
            }
        }
    }
    let name = name.ok_or_else(missing)?;
    check_name(&name)?;
    check_description(&description.ok_or_else(missing)?)?;
    let mut text = text.trim_start().to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(Skill { name, text })
}

/// `path` with every symlink and `..` resolved; a path that does not exist yet resolves
/// through its nearest existing folder.
fn real(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(error) => {
                let Some(Component::Normal(name)) = existing.components().next_back() else {
                    return Err(format!("{}: {error}", path.display()));
                };
                missing.push(name);
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("{}: {error}", path.display()))?;
            }
        }
    }
}

/// Writes `skill` under `base` and returns its folder. With `keep`, an existing skill of
/// that name is an error. Unless `allow_outside`, the folder must resolve under `base`
/// (a link in the library cannot send the write elsewhere).
pub(crate) fn write(
    base: &Path,
    skill: &Skill,
    keep: bool,
    allow_outside: bool,
    max_bytes: usize,
) -> Result<PathBuf, String> {
    if skill.text.len() > max_bytes {
        return Err(format!(
            "skill `{}` has {} bytes, more than `max_bytes` {max_bytes}",
            skill.name,
            skill.text.len()
        ));
    }
    fs::create_dir_all(base).map_err(|e| format!("{}: {e}", base.display()))?;
    let root = base
        .canonicalize()
        .map_err(|e| format!("{}: {e}", base.display()))?;
    let folder = real(&root.join(&skill.name))?;
    if !allow_outside && !folder.starts_with(&root) {
        return Err(format!(
            "{} leaves the library {} (`allow_outside`)",
            folder.display(),
            root.display()
        ));
    }
    let entry = folder.join(ENTRY);
    if keep && entry.exists() {
        return Err(format!(
            "skill `{}` exists and `existing = keep`",
            skill.name
        ));
    }
    fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let temp = folder.join(format!(".{ENTRY}.{}-{n}.regulus-tmp", std::process::id()));
    let written = fs::write(&temp, &skill.text).and_then(|()| fs::rename(&temp, &entry));
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!("{}: {error}", entry.display()));
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_and_text_skills_become_a_skill_md() {
        let skill = parse(
            &json!({ "name": "run-tests", "description": "How to run\nthe tests",
                                   "body": "Run `cargo test`." }),
        )
        .unwrap();
        assert_eq!(skill.name, "run-tests");
        assert_eq!(
            skill.text,
            "---\nname: run-tests\ndescription: \"How to run the tests\"\n---\n\nRun `cargo test`.\n"
        );
        // Its own output parses back as a text skill.
        assert_eq!(parse(&json!(skill.text)).unwrap().name, "run-tests");
        let text = "---\nname: deploy\ndescription: 'Ship it'\n---\nSteps.";
        assert_eq!(parse(&json!(text)).unwrap().text, format!("{text}\n"));
        assert!(parse(&json!("just words")).is_err());
        assert!(parse(&json!({ "name": "../x", "description": "d", "body": "b" })).is_err());
        assert!(parse(&json!({ "name": "Up", "description": "d", "body": "b" })).is_err());
        assert!(parse(&json!({ "name": "x", "description": " ", "body": "b" })).is_err());
        assert!(parse(&json!({ "name": "x", "description": "d" })).is_err());
    }

    #[test]
    fn skills_land_in_their_folder_and_links_out_are_refused() {
        let root = std::env::temp_dir().join(format!("regulus-library-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("lib");
        let skill = parse(&json!({ "name": "a", "description": "d", "body": "b" })).unwrap();
        let folder = write(&base, &skill, false, false, 1000).unwrap();
        assert!(folder.join(ENTRY).is_file());
        assert!(write(&base, &skill, true, false, 1000).is_err(), "keep");
        assert!(write(&base, &skill, false, false, 5).is_err(), "max_bytes");
        #[cfg(unix)]
        {
            fs::create_dir_all(root.join("elsewhere")).unwrap();
            std::os::unix::fs::symlink(root.join("elsewhere"), base.join("b")).unwrap();
            let out = parse(&json!({ "name": "b", "description": "d", "body": "b" })).unwrap();
            assert!(write(&base, &out, false, false, 1000).is_err());
            assert!(write(&base, &out, false, true, 1000).is_ok());
        }
    }
}
