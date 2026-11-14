//! Calculator — port of `plugins/calculator.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python help entry `help_calculator`):
//! - `.calc` — open the inline calculator keypad.
//! - `.calc <expression>` — evaluate an expression directly (Rust-side
//!   convenience matching the keypad's `=` behaviour; Python's `.calc`
//!   takes no argument).
//!
//! Python's `.calc` with no argument opens an inline keypad through the
//! assistant bot (`inline_query(asst, "calc")` + click) and the keypad
//! itself is driven by `@callback("calc…")` / `@callback("recalc")` on
//! the assistant — inline queries, clicks, callback answers and button
//! edits are assistant-crate surface, not user-plugin commands, so they
//! are not registered here. The expression evaluator is the real
//! calculation core of the keypad's `=` button (Python `eval(get)`),
//! re-implemented as a small safe arithmetic parser (no `eval`);
//! it supports `+ - * / % ( )` and decimals, exactly the keys the
//! keypad offers (`+ - x ÷ % .` digits, `00`).
//!
//! Per-user partial expressions (Python's `CALC` dict) live in udB
//! under `calc:<sender_id>` so the state survives restarts.

use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def("calc", "calc", "Calculator: .calc [expression]."),
        |ctx| Box::pin(calc(ctx)),
    );
}

fn calc_key(ctx: &Ctx) -> String {
    format!("calc:{}", ctx.sender_id)
}

/// Safe arithmetic evaluator: `+ - * / % ( )`, decimals, unary minus.
/// Mirrors what the keypad's `=` button evaluates (Python `eval(get)`).
fn eval_expr(input: &str) -> Result<f64> {
    let mut p = Parser {
        chars: input.chars().filter(|c| !c.is_whitespace()).collect(),
        pos: 0,
    };
    let v = p.parse_sum()?;
    if p.pos != p.chars.len() {
        return Err(Error::Other(format!(
            "could not parse expression: unexpected '{}'",
            p.chars[p.pos]
        )));
    }
    Ok(v)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn parse_sum(&mut self) -> Result<f64> {
        let mut v = self.parse_term()?;
        loop {
            match self.peek() {
                Some('+') => {
                    self.next();
                    v += self.parse_term()?;
                }
                Some('-') => {
                    self.next();
                    v -= self.parse_term()?;
                }
                _ => return Ok(v),
            }
        }
    }

    fn parse_term(&mut self) -> Result<f64> {
        let mut v = self.parse_factor()?;
        loop {
            match self.peek() {
                Some('*') | Some('x') | Some('×') => {
                    self.next();
                    v *= self.parse_factor()?;
                }
                Some('/') | Some('÷') => {
                    self.next();
                    let d = self.parse_factor()?;
                    if d == 0.0 {
                        return Err(Error::Other("division by zero".into()));
                    }
                    v /= d;
                }
                Some('%') => {
                    self.next();
                    v /= 100.0;
                }
                _ => return Ok(v),
            }
        }
    }

    fn parse_factor(&mut self) -> Result<f64> {
        match self.peek() {
            Some('-') => {
                self.next();
                Ok(-self.parse_factor()?)
            }
            Some('+') => {
                self.next();
                self.parse_factor()
            }
            Some('(') => {
                self.next();
                let v = self.parse_sum()?;
                if self.next() != Some(')') {
                    return Err(Error::Other("missing closing ')'".into()));
                }
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.parse_number(),
            Some(c) => Err(Error::Other(format!(
                "could not parse expression: unexpected '{c}'"
            ))),
            None => Err(Error::Other(
                "could not parse expression: unexpected end".into(),
            )),
        }
    }

    fn parse_number(&mut self) -> Result<f64> {
        let mut s = String::new();
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            s.push(self.next().unwrap());
        }
        s.parse::<f64>()
            .map_err(|_| Error::Other(format!("could not parse number '{s}'")))
    }
}

async fn calc(ctx: &Ctx) -> Result<()> {
    let expr = ctx.args.trim();
    if expr.is_empty() {
        // Python: inline-query the assistant bot for "calc" and click the
        // result (Python: calculator.py:icalc). The keypad state is stored
        // in udB so a wired keypad can resume it.
        let _ = db_get_json::<String>(&*ctx.db, &calc_key(ctx)).await?;
        let Some(asst_id) = ctx.asst_dialog_id else {
            return Err(Error::Other(
                "opening the inline calculator keypad requires the assistant bot to be connected"
                    .into(),
            ));
        };
        ctx.responder
            .inline_query_click(
                asst_id,
                ctx.asst_username.as_deref(),
                "calc",
                ctx.chat_id,
                None,
            )
            .await?;
        return Ok(());
    }
    // Keypad-style normalisation: `x`/`÷` are the keypad's `*`/`/`.
    let normalised = expr.replace('x', "*").replace('÷', "/");
    // A trailing operator is dropped before evaluating, like Python's `=`.
    let trimmed = normalised
        .trim_end_matches(['*', '.', '/', '-', '+'])
        .to_string();
    match eval_expr(&trimmed) {
        Ok(v) => {
            // Clear the stored partial expression, like Python's `=` flow.
            let _ = ctx.db.del(&calc_key(ctx)).await;
            ctx.reply(&format!("Answer : {v}")).await?;
        }
        Err(e) => {
            ctx.reply(&format!("Could not evaluate: {e}")).await?;
        }
    }
    Ok(())
}

/// Apply one keypad key to the stored partial expression and return the
/// new display text — the real state machine behind the Python
/// `@callback("calc…")` handler, kept for the Responder wiring pass.
/// Keys: digits, `00`, `.`, `+`, `-`, `x`, `÷`, `%`, `⌫`, `C`, `AC`, `=`.
#[allow(dead_code)]
async fn keypad_press(ctx: &Ctx, key: &str) -> Result<Option<String>> {
    let k = calc_key(ctx);
    let mut cur: String = db_get_json::<String>(&*ctx.db, &k)
        .await?
        .unwrap_or_default();
    match key {
        "AC" | "C" => {
            cur.clear();
            let _ = ctx.db.del(&k).await;
            return Ok(Some("cleared".to_string()));
        }
        "⌫" => {
            cur.pop();
        }
        "%" => cur.push_str("/100"),
        "÷" => cur.push('/'),
        "x" => cur.push('*'),
        "=" => {
            let trimmed = cur.trim_end_matches(['*', '.', '/', '-', '+']).to_string();
            let v = eval_expr(&trimmed)?;
            let _ = ctx.db.del(&k).await;
            return Ok(Some(format!("Answer : {v}")));
        }
        _ => cur.push_str(key),
    }
    db_set_json(&*ctx.db, &k, &cur).await?;
    Ok(Some(cur))
}
