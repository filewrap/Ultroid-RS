//! Akinator client — port of vendored `akipy/akinator.py`.
//!
//! Real HTTP calls to the Akinator web game (`https://{lang}.akinator.com`),
//! same endpoints and payloads as akipy: `GET /` (region check),
//! `POST /game` (HTML: session/signature tokens + first question),
//! `POST /answer`, `POST /cancel_answer`.

use crate::error::{Error, Result};
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AkinatorQuestion {
    pub question: String,
    pub step: i32,
    pub progress: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AkinatorGuess {
    pub name: String,
    pub description: String,
    pub probability: f64,
    pub picture_path: Option<String>,
}

pub enum AkinatorStep {
    Question(AkinatorQuestion),
    Guess(AkinatorGuess),
}

pub struct Akinator {
    http: Client,
    lang: String,
    base: Option<String>,
    session: Option<String>,
    signature: Option<String>,
    step: i32,
    progression: String,
    step_last_proposition: String,
}

fn extract_base64_tokens(html: &str) -> Vec<String> {
    // akipy: re.findall(r"[a-zA-Z0-9+/]+==", req) — first two are
    // session and signature.
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in html.chars() {
        if c.is_ascii_alphanumeric() || c == '+' || c == '/' {
            cur.push(c);
        } else {
            if cur.ends_with("==") && cur.len() > 4 {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.ends_with("==") && cur.len() > 4 {
        out.push(cur);
    }
    out
}

fn extract_question(html: &str) -> Option<String> {
    let marker = r#"<p class="question-text" id="question-label">"#;
    let start = html.find(marker)? + marker.len();
    let end = html[start..].find("</p>")? + start;
    Some(html[start..end].to_string())
}

impl Akinator {
    pub fn new(lang: impl Into<String>) -> Self {
        Akinator {
            http: Client::new(),
            lang: lang.into(),
            base: None,
            session: None,
            signature: None,
            step: 0,
            progression: "0.00000".to_string(),
            step_last_proposition: String::new(),
        }
    }

    pub async fn start_game(&mut self, child_mode: bool) -> Result<AkinatorQuestion> {
        let base = format!("https://{}.akinator.com", self.lang);
        let resp = self.http.get(&base).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!("akinator: {base} returned {}", resp.status())));
        }
        self.base = Some(base.clone());
        // akipy `__initialise`: POST /game with sid=theme(1), cm=child mode.
        let form = [("sid", "1"), ("cm", if child_mode { "true" } else { "false" })];
        let html = self
            .http
            .post(format!("{base}/game"))
            .form(&form)
            .send()
            .await?
            .text()
            .await?;
        let tokens = extract_base64_tokens(&html);
        if tokens.len() >= 2 {
            self.session = Some(tokens[0].clone());
            self.signature = Some(tokens[1].clone());
        }
        let question = extract_question(&html)
            .ok_or_else(|| Error::Other("akinator: could not read first question".into()))?;
        self.step = 0;
        self.progression = "0.00000".to_string();
        Ok(AkinatorQuestion {
            question,
            step: 0,
            progress: 0.0,
        })
    }

    /// Answer the current question. akipy answer ids: 0=yes, 1=no,
    /// 2=don't know, 3=probably, 4=probably not.
    pub async fn answer(&mut self, option: i32) -> Result<AkinatorStep> {
        let base = self.base.clone().ok_or_else(|| Error::Other("akinator not started".into()))?;
        let form = [
            ("step", self.step.to_string()),
            ("progression", self.progression.clone()),
            ("sid", "1".to_string()),
            ("cm", "false".to_string()),
            ("answer", option.to_string()),
            ("step_last_proposition", self.step_last_proposition.clone()),
            ("session", self.session.clone().unwrap_or_default()),
            ("signature", self.signature.clone().unwrap_or_default()),
        ];
        let text = self
            .http
            .post(format!("{base}/answer"))
            .form(&form)
            .send()
            .await?
            .text()
            .await?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| Error::Other(format!("akinator bad json: {e}")))?;
        if v.get("id_proposition").is_some() {
            // Win: name_proposition / description_proposition / photo.
            let probability: f64 = v
                .get("proba")
                .and_then(|p| p.as_str())
                .and_then(|s| s.parse().ok())
                .or_else(|| v.get("proba").and_then(|p| p.as_f64()))
                .unwrap_or(0.0);
            return Ok(AkinatorStep::Guess(AkinatorGuess {
                name: v
                    .get("name_proposition")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: v
                    .get("description_proposition")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                probability,
                picture_path: v.get("photo").and_then(|p| p.as_str()).map(|s| s.to_string()),
            }));
        }
        self.step = v.get("step").and_then(|s| s.as_i64()).unwrap_or(self.step as i64 + 1) as i32;
        self.progression = v
            .get("progression")
            .and_then(|p| p.as_str())
            .unwrap_or("0")
            .to_string();
        let question = v
            .get("question")
            .and_then(|q| q.as_str())
            .unwrap_or("")
            .to_string();
        let progress: f64 = self.progression.parse().unwrap_or(0.0);
        Ok(AkinatorStep::Question(AkinatorQuestion {
            question,
            step: self.step,
            progress,
        }))
    }

    pub async fn back(&mut self) -> Result<()> {
        if self.step <= 1 {
            return Err(Error::Other("akinator: cannot go back further".into()));
        }
        let base = self.base.clone().ok_or_else(|| Error::Other("akinator not started".into()))?;
        let form = [
            ("step", self.step.to_string()),
            ("progression", self.progression.clone()),
            ("sid", "1".to_string()),
            ("cm", "false".to_string()),
            ("session", self.session.clone().unwrap_or_default()),
            ("signature", self.signature.clone().unwrap_or_default()),
        ];
        let text = self
            .http
            .post(format!("{base}/cancel_answer"))
            .form(&form)
            .send()
            .await?
            .text()
            .await?;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
        self.step = v.get("step").and_then(|s| s.as_i64()).unwrap_or(self.step as i64 - 1) as i32;
        self.progression = v
            .get("progression")
            .and_then(|p| p.as_str())
            .unwrap_or(&self.progression)
            .to_string();
        Ok(())
    }
}
