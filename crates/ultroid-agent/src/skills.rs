//! Declarative agent skills — port of `pyUltroid/agent/_skills.py`.
//!
//! Skills are small, auditable capability descriptions injected into the
//! system prompt. They are data, not arbitrary code, so a skill can be
//! enabled per chat without granting new OS capabilities.

/// One declarative skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: &'static str,
    pub summary: &'static str,
    pub guidance: &'static str,
    pub surfaces: &'static [&'static str],
}

pub static SKILLS: &[Skill] = &[
    Skill {
        name: "research",
        summary: "Search the web and summarize sources.",
        guidance: "Prefer web_search for current or niche facts. State uncertainty and cite the URLs you relied on.",
        surfaces: &[".ai", ".ask", ".agent", ".aichat", "dm"],
    },
    Skill {
        name: "diagnostics",
        summary: "Inspect traces, code, and runtime status before guessing.",
        guidance: "Use get_traces and code_search before proposing a fix for a plugin or command. Report the observed cause separately from the proposed change.",
        surfaces: &[".agent", "dm"],
    },
    Skill {
        name: "automation",
        summary: "Use the Telegram and Ultroid systems to finish a task.",
        guidance: "When a task changes external state, confirm the target first and report the result with the concrete identifiers it produced.",
        surfaces: &[".agent"],
    },
];

/// The skills that apply to `surface`, or all of them when `None`.
pub fn available_skills(surface: Option<&str>) -> Vec<&'static Skill> {
    match surface {
        None => SKILLS.iter().collect(),
        Some(s) => SKILLS
            .iter()
            .filter(|skill| skill.surfaces.contains(&s))
            .collect(),
    }
}

/// Names of the skills that apply to `surface`.
pub fn skill_names(surface: Option<&str>) -> Vec<&'static str> {
    available_skills(surface)
        .into_iter()
        .map(|skill| skill.name)
        .collect()
}

/// The system-prompt block for `surface`, or `""` when nothing applies.
pub fn render_skills(surface: Option<&str>) -> String {
    let skills = available_skills(surface);
    if skills.is_empty() {
        return String::new();
    }
    let mut lines = vec!["Available skills:".to_string()];
    for skill in skills {
        lines.push(format!(
            "- {}: {} {}",
            skill.name, skill.summary, skill.guidance
        ));
    }
    lines.join("\n")
}
