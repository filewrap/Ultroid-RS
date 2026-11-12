//! Fancy fonts — port of `plugins/fontgen.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python help entry `help_fontgen`):
//! - `.font` — list available fonts.
//! - `.font <font> : <text>` — convert text to the named font.
//! - `.font <font>` (reply to text) — convert the replied text.
//!
//! The conversion is pure text mapping, fully implemented. The four
//! fonts are ported verbatim from the Python `Fonts` dict (`small caps`,
//! `monospace`, `double stroke`, `script royal`).
//!
//! Quirk preserved from Python (`fontgen.py:_`): when the input has no
//! reply, the font part is taken as `input.split(":", 1)[0][:-1]` — the
//! last character of the font name is dropped. So the inline form that
//! works is e.g. `.font monospace : hello` (note the space before the
//! colon), while `.font monospace: hello` looks up `monospac` and fails
//! with "not in font list".

use std::collections::BTreeMap;
use ultroid_core::{Ctx, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "font",
            "font( (.*)|$)",
            "Fancy fonts: .font | .font <font> : <text> | .font <font> (reply).",
        ),
        |ctx| Box::pin(font(ctx)),
    );
}

const DEFAULT_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

fn fonts() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (
            "small caps",
            "ᴀʙᴄᴅᴇғɢʜɪᴊᴋʟᴍɴᴏᴘϙʀsᴛᴜᴠᴡxʏᴢABCDEFGHIJKLMNOPQRSTUVWXYZ",
        ),
        (
            "monospace",
            "𝚊𝚋𝚌𝚍𝚎𝚏𝚐𝚑𝚒𝚓𝚔𝚕𝚖𝚗𝚘𝚙𝚚𝚛𝚜𝚝𝚞𝚟𝚠𝚡𝚢𝚣𝙰𝙱𝙲𝙳𝙴𝙵𝙶𝙷𝙸𝙹𝙺𝙻𝙼𝙽𝙾𝙿𝚀𝚁𝚂𝚃𝚄𝚅𝚆𝚇𝚈𝚉",
        ),
        (
            "double stroke",
            "𝕒𝕓𝕔𝕕𝕖𝕗𝕘𝕙𝕚𝕛𝕜𝕝𝕞𝕟𝕠𝕡𝕢𝕣𝕤𝕥𝕦𝕧𝕨𝕩𝕪𝕫𝔸𝔹ℂ𝔻𝔼𝔽𝔾ℍ𝕀𝕁𝕂𝕃𝕄ℕ𝕆ℙℚℝ𝕊𝕋𝕌𝕍𝕎𝕏𝕐ℤ",
        ),
        (
            "script royal",
            "𝒶𝒷𝒸𝒹𝑒𝒻𝑔𝒽𝒾𝒿𝓀𝓁𝓂𝓃𝑜𝓅𝓆𝓇𝓈𝓉𝓊𝓋𝓌𝓍𝓎𝓏𝒜ℬ𝒞𝒟ℰℱ𝒢ℋℐ𝒥𝒦ℒℳ𝒩𝒪𝒫𝒬ℛ𝒮𝒯𝒰𝒱𝒲𝒳𝒴𝒵",
        ),
    ])
}

/// Python `gen_font`: for each ASCII letter, in order, replace every
/// occurrence in the text with the mapped glyph.
fn gen_font(text: &str, new_font: &str) -> String {
    // The Python mapping is a plain char-for-char zip of the font string
    // against ascii_letters.
    let font_chars: Vec<char> = new_font.chars().collect();
    let default_chars: Vec<char> = DEFAULT_CHARS.chars().collect();
    let mut out = text.to_string();
    for (i, &q) in default_chars.iter().enumerate() {
        if let Some(&replacement) = font_chars.get(i) {
            out = out.replace(q, &replacement.to_string());
        }
    }
    out
}

async fn font(ctx: &Ctx) -> Result<()> {
    let input = ctx.args.trim().to_string();
    let fonts = fonts();
    if input.is_empty() {
        let mut m = "**Available Fonts**\n\n".to_string();
        for name in fonts.keys() {
            m.push_str(&format!("• `{name}`\n"));
        }
        ctx.reply(&m).await?;
        return Ok(());
    }
    let (font_name, text): (String, String) = if let Some(reply) = &ctx.reply_to_text {
        (input.clone(), reply.clone())
    } else {
        // No reply: split on the first ":"; the font part loses its last
        // character (Python `_[0][:-1]` quirk — see module docs).
        match input.split_once(':') {
            Some((font_part, text)) => {
                let mut chars: Vec<char> = font_part.chars().collect();
                chars.pop();
                (chars.into_iter().collect(), text.to_string())
            }
            None => {
                ctx.reply("`Give font : text` — usage: `.font <font> : <text>`")
                    .await?;
                return Ok(());
            }
        }
    };
    let Some(mapping) = fonts.get(font_name.trim()) else {
        ctx.reply(&format!("`{font_name} not in font list`."))
            .await?;
        return Ok(());
    };
    let converted = gen_font(&text, mapping);
    ctx.reply(&converted).await?;
    Ok(())
}
