//! The two read-only skill tools (design §8): read a skill's `SKILL.md` body
//! and read files beside it. `ducky__fs_*` is sandboxed to the working
//! directory, so these are the only way to reach a plugin's own files.

use std::path::Path;

use serde_json::Value;

use crate::plugins::path::resolve_within;
use crate::plugins::skills::{cap_file, load_body, ResolvedSkill};

pub const LOAD_SKILL: &str = "ducky__load_skill";
pub const READ_SKILL_FILE: &str = "ducky__read_skill_file";

/// Whether `name` is one of the two skill tools.
pub fn is_skill_tool(name: &str) -> bool {
    name == LOAD_SKILL || name == READ_SKILL_FILE
}

/// Resolve a tool argument to a skill: an exact id first, else a bare name
/// through the precedence order (`skills` is already in precedence order, so
/// the first name match is the winner).
pub fn resolve_id(skills: &[ResolvedSkill], id_or_name: &str) -> Option<ResolvedSkill> {
    let query = id_or_name.trim();
    if query.is_empty() {
        return None;
    }
    if let Some(skill) = skills.iter().find(|skill| skill.id == query) {
        return Some(skill.clone());
    }
    skills.iter().find(|skill| skill.name == query).cloned()
}

/// Run one skill tool. `skills` is the precedence-ordered list for this turn.
///
/// The engine calls this directly (the tools need the resolved list, not the
/// working directory); [`crate::builtin::execute`] never dispatches them.
pub fn execute(name: &str, args: &Value, skills: &[ResolvedSkill]) -> Result<String, String> {
    let Some(id) = args.get("id").and_then(Value::as_str) else {
        return Err(format!("{name} needs an `id` argument"));
    };
    let Some(skill) = resolve_id(skills, id) else {
        return Err(unknown_skill(id, skills));
    };
    match name {
        LOAD_SKILL => load_skill(&skill),
        READ_SKILL_FILE => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return Err(format!("{READ_SKILL_FILE} needs a `path` argument"));
            };
            read_skill_file(&skill, path)
        }
        _ => Err(format!("Unknown skill tool: {name}")),
    }
}

/// The `SKILL.md` body, prefixed with where the skill lives.
fn load_skill(skill: &ResolvedSkill) -> Result<String, String> {
    let body = load_body(skill)?;
    let mut text = format!(
        "Skill `{}` (directory: {}).\n\
         Files beside the skill — `scripts/`, `references/` — are read with \
         {READ_SKILL_FILE} using the id `{}` and a path relative to that directory.\n\n",
        skill.id,
        skill.dir.display(),
        skill.id
    );
    if skill.shadowed.is_some() {
        text.push_str(
            "Note: another root provides a skill with this name; this is the shadowed \
             copy you asked for by id.\n\n",
        );
    }
    text.push_str(&body);
    Ok(text)
}

/// One file inside the skill directory, capped at 32 KB. `..`, absolute paths
/// and symlink escapes are refused; a symlink that stays inside is allowed.
pub fn read_skill_file(skill: &ResolvedSkill, path: &str) -> Result<String, String> {
    if path.trim().is_empty() || Path::new(path).is_absolute() {
        return Err(format!(
            "`{path}` must be a relative path inside the skill directory"
        ));
    }
    let target = skill.dir.join(path);
    if !target.exists() {
        return Err(format!(
            "cannot read `{path}`: no such file in the skill directory"
        ));
    }
    let resolved = resolve_within(&skill.dir, &target)
        .ok_or_else(|| format!("`{path}` is outside the skill directory"))?;
    let text = std::fs::read_to_string(&resolved)
        .map_err(|err| format!("cannot read `{path}`: {err}"))?;
    Ok(cap_file(&text))
}

/// An unknown id is a retry hint: name the closest ids instead of failing flat.
fn unknown_skill(asked: &str, skills: &[ResolvedSkill]) -> String {
    let mut ranked: Vec<&ResolvedSkill> = skills.iter().collect();
    ranked.sort_by_key(|skill| (edit_distance(asked, &skill.name), skill.id.clone()));
    if ranked.is_empty() {
        format!("unknown skill \"{asked}\": no skills are installed. Run /skills to check.")
    } else {
        let close: Vec<&str> = ranked
            .iter()
            .take(3)
            .map(|skill| skill.id.as_str())
            .collect();
        format!("unknown skill \"{asked}\". Closest ids: {}", close.join(", "))
    }
}

/// Plain Levenshtein distance; the skill lists are small (the prompt block
/// caps at 60 entries).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}
