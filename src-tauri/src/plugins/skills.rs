//! Skill roots, precedence, ids and the system-prompt block (design §4, §8).
//!
//! The four roots are scanned in precedence order — user, workspace, agents,
//! plugin — and the first skill with a given name wins. Shadowed duplicates
//! stay in the list, marked with the id that outranked them, so the model can
//! still load one by its full id.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::layout::parse_frontmatter;
use super::manager::PluginIndex;

/// How many skills the prompt block lists before it truncates.
pub const PROMPT_CAP: usize = 60;
/// Longest skill description shown in the prompt block, in characters.
const DESC_CAP: usize = 200;
/// Largest `SKILL.md` body or skill file the tools return, in bytes.
pub const FILE_CAP: usize = 32 * 1024;
/// Appended when a file is cut at [`FILE_CAP`]; counted inside the cap.
const TRUNCATION_NOTICE: &str = "\n\n[truncated: the file is larger than 32 KB]";

/// Where a skill came from, in precedence order (design §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillOrigin {
    User,
    Workspace,
    Agents,
    Plugin { id: String, name: String },
}

impl SkillOrigin {
    /// The stable id the tools accept: `user:<name>`, `workspace:<name>`,
    /// `agents:<name>` or `plugin:<plugin-id>:<name>`.
    pub(crate) fn id_for(&self, name: &str) -> String {
        match self {
            SkillOrigin::User => format!("user:{name}"),
            SkillOrigin::Workspace => format!("workspace:{name}"),
            SkillOrigin::Agents => format!("agents:{name}"),
            SkillOrigin::Plugin { id, .. } => format!("plugin:{id}:{name}"),
        }
    }

    /// How the prompt block names the origin.
    fn label(&self) -> String {
        match self {
            SkillOrigin::User => "user".to_string(),
            SkillOrigin::Workspace => "workspace".to_string(),
            SkillOrigin::Agents => "agents".to_string(),
            SkillOrigin::Plugin { name, .. } => format!("plugin: {name}"),
        }
    }
}

/// One skill after precedence and shadowing are applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSkill {
    /// `user:<name>`, `workspace:<name>`, `agents:<name>` or
    /// `plugin:<plugin-id>:<name>`.
    pub id: String,
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub origin: SkillOrigin,
    /// The id of the skill that won the name, when this one is shadowed.
    pub shadowed: Option<String>,
}

/// Collect every skill in precedence order: user, workspace, agents, plugin.
///
/// The first skill with a given name wins; later duplicates are still listed
/// with `shadowed` set to the winner's id. `enabled` is the master switch:
/// when false the result is empty, so neither the prompt block nor the tools
/// see anything.
pub fn collect(
    index: &PluginIndex,
    user: &Path,
    workspace: &Path,
    agents: &Path,
    enabled: bool,
) -> Vec<ResolvedSkill> {
    if !enabled {
        return Vec::new();
    }
    let mut found: Vec<(String, String, PathBuf, SkillOrigin)> = Vec::new();
    for (root, origin) in [
        (user, SkillOrigin::User),
        (workspace, SkillOrigin::Workspace),
        (agents, SkillOrigin::Agents),
    ] {
        for (name, description, dir) in scan_root(root) {
            found.push((name, description, dir, origin.clone()));
        }
    }
    for plugin in index.plugins.iter().filter(|plugin| plugin.enabled) {
        for skill in &plugin.skills {
            found.push((
                skill.name.clone(),
                skill.description.clone(),
                PathBuf::from(&skill.path),
                SkillOrigin::Plugin {
                    id: plugin.id.clone(),
                    name: plugin.name.clone(),
                },
            ));
        }
    }

    let mut winners: HashMap<String, String> = HashMap::new();
    let mut skills = Vec::with_capacity(found.len());
    for (name, description, dir, origin) in found {
        let id = origin.id_for(&name);
        let shadowed = winners.get(&name).cloned();
        if shadowed.is_none() {
            winners.insert(name.clone(), id.clone());
        }
        skills.push(ResolvedSkill {
            id,
            name,
            description,
            dir,
            origin,
            shadowed,
        });
    }
    skills
}

/// One root's immediate child skill directories: `<name>/SKILL.md` with valid
/// frontmatter whose `name` matches the directory. Invalid skills are
/// skipped, matching the plugin layer's failure boundary (design §4).
fn scan_root(root: &Path) -> Vec<(String, String, PathBuf)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(dir.join("SKILL.md")) else {
            continue;
        };
        let Some((frontmatter, _)) = parse_frontmatter(&text) else {
            continue;
        };
        let skill_name = frontmatter.get("name").filter(|name| !name.is_empty());
        let description = frontmatter.get("description").filter(|d| !d.is_empty());
        let (Some(skill_name), Some(description)) = (skill_name, description) else {
            continue;
        };
        if skill_name != name {
            continue;
        }
        found.push((skill_name.clone(), description.clone(), dir));
    }
    found
}

/// The system-prompt block listing skills, or `None` when there is nothing to
/// list. `None` keeps a plugin-free Ducky's prompt byte-identical.
pub fn prompt_block(skills: &[ResolvedSkill], cap: usize) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut block = String::from(
        "# Skills\n\
         Skills add instructions for a specific kind of task. When a task matches \
         a skill's description, call ducky__load_skill with the name or id shown \
         below to read the full instructions; files beside the skill (scripts, \
         references) are read with ducky__read_skill_file.\n",
    );
    for skill in skills.iter().take(cap) {
        // a bare name is unambiguous only when no other root uses that name
        let shown = if skills.iter().filter(|s| s.name == skill.name).count() == 1 {
            skill.name.clone()
        } else {
            skill.id.clone()
        };
        block.push_str(&format!(
            "- `{shown}` — {} ({})",
            truncate_chars(&skill.description, DESC_CAP),
            skill.origin.label()
        ));
        if let Some(winner) = &skill.shadowed {
            block.push_str(&format!(" [shadowed by {winner}]"));
        }
        block.push('\n');
    }
    let omitted = skills.len().saturating_sub(cap);
    if omitted > 0 {
        block.push_str(&format!(
            "{omitted} more skill(s) are not shown here; run /skills to list them all.\n"
        ));
    }
    Some(block)
}

/// The `SKILL.md` body with frontmatter stripped, capped at 32 KB.
pub fn load_body(skill: &ResolvedSkill) -> Result<String, String> {
    let path = skill.dir.join("SKILL.md");
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let body = match parse_frontmatter(&text) {
        Some((_, body)) => body,
        None => text,
    };
    Ok(cap_file(&body))
}

/// Cap a file's text at [`FILE_CAP`] bytes, on a character boundary, ending
/// with the truncation notice when something was cut.
pub(crate) fn cap_file(text: &str) -> String {
    if text.len() <= FILE_CAP {
        return text.to_string();
    }
    let keep = FILE_CAP.saturating_sub(TRUNCATION_NOTICE.len());
    let mut out = truncate_to_bytes(text, keep).to_string();
    out.push_str(TRUNCATION_NOTICE);
    out
}

/// The longest prefix of `text` that is at most `max` bytes; never splits a
/// multi-byte character.
fn truncate_to_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        let next = index + ch.len_utf8();
        if next > max {
            break;
        }
        end = next;
    }
    &text[..end]
}

/// Truncate `text` to at most `cap` characters.
fn truncate_chars(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        text.to_string()
    } else {
        text.chars().take(cap).collect()
    }
}
