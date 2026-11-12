//! The setup UI, as one self-contained page.
//!
//! A port of the *meaning* of `resources/web/index.html` + `setup.js` +
//! `style.css` — the same three steps (API credentials → Telegram login
//! → finish) driving the same `/api/*` routes — rewritten, not copied.
//! The page is embedded in the binary (`include_str!` equivalent: this
//! `&str` constant), so the setup server never depends on files on disk
//! the way Python's `StaticFiles` mount did.

/// The whole setup page: HTML + CSS + JS in one document.
pub const SETUP_PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>SarahUB Setup</title>
<style>
  * { box-sizing: border-box; }
  body { margin: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: #f6f6fb; color: #17181a; padding: 16px; line-height: 1.45; }
  .wrap { max-width: 640px; margin: 0 auto; }
  h1 { font-size: 20px; margin: 6px 0 4px; }
  .muted { color: #8e8e93; font-size: 13px; }
  .steps { display: flex; gap: 8px; list-style: none; padding: 0; margin: 14px 0; }
  .step { flex: 1; padding: 8px 10px; border: 1px solid rgba(120,120,128,.18); border-radius: 10px; background: #fff; font-size: 13px; }
  .step.is-current { border-color: #5b5bf5; box-shadow: 0 0 0 1px #5b5bf5 inset; }
  .step.is-done { opacity: .6; }
  .card { background: #fff; border: 1px solid rgba(120,120,128,.14); border-radius: 14px; padding: 14px 16px; margin-bottom: 12px; box-shadow: 0 1px 2px rgba(20,20,40,.04); }
  .card h2 { font-size: 16px; margin: 0 0 8px; }
  .hidden { display: none !important; }
  label { display: block; font-size: 13px; font-weight: 600; margin: 10px 0 4px; }
  input { width: 100%; padding: 9px 10px; border: 1px solid rgba(120,120,128,.35); border-radius: 9px; font: inherit; font-size: 14px; }
  .btn { border: 1px solid transparent; border-radius: 999px; padding: 8px 18px; font: inherit; font-size: 14px; font-weight: 600; background: #5b5bf5; color: #fff; cursor: pointer; margin-top: 12px; }
  .btn-secondary { background: rgba(120,120,128,.16); color: #17181a; }
  .btn:active { opacity: .85; }
  .tabs { display: flex; gap: 8px; margin-bottom: 10px; }
  .tab { border: 1px solid rgba(120,120,128,.35); background: #fff; border-radius: 999px; padding: 6px 14px; font: inherit; font-size: 13px; cursor: pointer; }
  .tab.active { background: #5b5bf5; color: #fff; border-color: #5b5bf5; }
  .result { margin-top: 10px; }
  #log { background: #17181a; color: #d7d7de; border-radius: 10px; padding: 10px 12px; font-family: ui-monospace, "JetBrains Mono", Menlo, monospace; font-size: 12px; line-height: 1.5; max-height: 260px; overflow-y: auto; white-space: pre-wrap; word-break: break-word; }
  #qr-img { max-width: 280px; display: block; margin: 10px 0; }
  code.break { word-break: break-all; font-size: 12px; }
  #error { color: #cc2929; background: rgba(204,41,41,.08); border: 1px solid rgba(204,41,41,.2); border-radius: 10px; padding: 8px 12px; font-size: 13px; margin-bottom: 10px; display: none; }
</style>
</head>
<body>
<main class="wrap">
  <h1>SarahUB Setup</h1>
  <p id="status-line" class="muted">checking server&hellip;</p>
  <ol class="steps">
    <li class="step is-current" data-step="keys"><b>1</b> API credentials</li>
    <li class="step" data-step="login"><b>2</b> Log in</li>
    <li class="step" data-step="complete"><b>3</b> Finish</li>
  </ol>
  <div id="error"></div>

  <section class="card" id="step-keys">
    <h2>1 &middot; API credentials</h2>
    <p class="muted">Get <code>API_ID</code> / <code>API_HASH</code> from
      <a href="https://my.telegram.org/apps" target="_blank" rel="noreferrer">my.telegram.org/apps</a>.</p>
    <form id="keys-form">
      <label>API_ID <input name="api_id" inputmode="numeric" autocomplete="off" required></label>
      <label>API_HASH <input name="api_hash" autocomplete="off" required></label>
      <label>BOT_TOKEN <span class="muted">(optional)</span> <input name="bot_token" autocomplete="off"></label>
      <label>LOG_CHANNEL <span class="muted">(optional chat id)</span> <input name="log_channel" inputmode="numeric" autocomplete="off"></label>
      <button type="submit" class="btn">Save credentials</button>
    </form>
    <p id="keys-result" class="muted result"></p>
  </section>

  <section class="card hidden" id="step-login">
    <h2>2 &middot; Telegram login</h2>
    <div class="tabs">
      <button id="tab-phone" class="tab active" type="button">Phone code</button>
      <button id="tab-qr" class="tab" type="button">QR code</button>
    </div>
    <div id="pane-phone">
      <form id="phone-form">
        <label>Phone number <input name="phone" placeholder="+1 415 555 0132" autocomplete="off" required></label>
        <button type="submit" class="btn">Send code</button>
      </form>
      <form id="otp-form" class="hidden">
        <label>Login code <input name="code" inputmode="numeric" autocomplete="one-time-code" required></label>
        <label>2FA password <span class="muted">(only if asked)</span> <input name="password" type="password" autocomplete="off"></label>
        <button type="submit" class="btn">Sign in</button>
      </form>
      <p id="login-result" class="muted result"></p>
    </div>
    <div id="pane-qr" class="hidden">
      <button id="qr-start" type="button" class="btn btn-secondary">Generate QR login</button>
      <div id="qr-box" class="hidden">
        <img id="qr-img" class="hidden" alt="QR login code">
        <p class="muted">Scan with Telegram &rarr; Settings &rarr; Devices &rarr; Link Desktop Device.</p>
        <p><code id="qr-url" class="break"></code></p>
        <p id="qr-status" class="muted"></p>
      </div>
    </div>
  </section>

  <section class="card hidden" id="step-complete">
    <h2>3 &middot; Finish</h2>
    <p class="muted">Logged in. Completing restarts the bot with the new configuration.</p>
    <button id="complete-btn" type="button" class="btn">Complete setup &amp; start bot</button>
    <p id="complete-result" class="muted result"></p>
  </section>

  <section class="card">
    <h2>Live log</h2>
    <div id="log"></div>
  </section>
</main>
<script>
(function () {
  "use strict";
  var params = new URLSearchParams(window.location.search);
  var token = params.get("token") || "";
  if (token && window.history && window.history.replaceState) {
    try { window.history.replaceState(null, "", window.location.pathname); } catch (e) {}
  }
  function $(id) { return document.getElementById(id); }
  function showError(msg) { var el = $("error"); el.textContent = msg; el.style.display = "block"; }
  function say(el, msg) { el.textContent = msg; }
  function api(path, opts) {
    opts = opts || {};
    opts.headers = Object.assign({ "X-Setup-Token": token }, opts.headers || {});
    return fetch(path, opts).then(function (res) {
      if (res.status === 401) throw new Error("Unauthorized - reopen the setup link.");
      if (!res.ok) return res.json().then(function (body) {
        throw new Error(body.detail || ("Request failed (" + res.status + ")"));
      });
      return res.json();
    });
  }
  function setStep(name) {
    document.querySelectorAll(".step").forEach(function (li) {
      li.classList.toggle("is-current", li.dataset.step === name);
      li.classList.toggle("is-done", ["keys", "login", "complete"].indexOf(li.dataset.step) < ["keys", "login", "complete"].indexOf(name));
    });
  }
  function show(id) { $(id).classList.remove("hidden"); }
  function hide(id) { $(id).classList.add("hidden"); }

  api("/api/state").then(function (st) {
    say($("status-line"), st.completed ? "setup complete" : (st.has_session ? "logged in - finish setup" : (st.has_api_keys ? "credentials saved - log in" : "waiting for credentials")));
    if (st.has_api_keys) { hide("step-keys"); show("step-login"); setStep("login"); }
    if (st.has_session) { show("step-complete"); setStep("complete"); }
  }).catch(function (err) { showError(err.message); });

  $("keys-form").addEventListener("submit", function (ev) {
    ev.preventDefault();
    var fd = new FormData(ev.target);
    var body = {
      api_id: parseInt(fd.get("api_id"), 10),
      api_hash: (fd.get("api_hash") || "").trim(),
      bot_token: (fd.get("bot_token") || "").trim() || null,
      log_channel: fd.get("log_channel") ? parseInt(fd.get("log_channel"), 10) : null
    };
    api("/api/setup/keys", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) })
      .then(function () { say($("keys-result"), "credentials saved"); hide("step-keys"); show("step-login"); setStep("login"); })
      .catch(function (err) { showError(err.message); });
  });

  var loginId = null;
  $("phone-form").addEventListener("submit", function (ev) {
    ev.preventDefault();
    var phone = new FormData(ev.target).get("phone");
    api("/api/login/phone", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ phone: phone }) })
      .then(function (body) {
        loginId = body.login_id;
        say($("login-result"), body.code_type ? ("code sent (" + body.code_type + ") - check Telegram") : "code sent - check Telegram");
        show("otp-form");
      })
      .catch(function (err) { showError(err.message); });
  });
  $("otp-form").addEventListener("submit", function (ev) {
    ev.preventDefault();
    var fd = new FormData(ev.target);
    var body = { login_id: loginId, code: (fd.get("code") || "").trim(), password: (fd.get("password") || "") || null };
    api("/api/login/otp", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) })
      .then(function () { say($("login-result"), "signed in"); show("step-complete"); setStep("complete"); })
      .catch(function (err) { showError(err.message); });
  });

  $("tab-phone").addEventListener("click", function () {
    $("tab-phone").classList.add("active"); $("tab-qr").classList.remove("active");
    show("pane-phone"); hide("pane-qr");
  });
  $("tab-qr").addEventListener("click", function () {
    $("tab-qr").classList.add("active"); $("tab-phone").classList.remove("active");
    show("pane-qr"); hide("pane-phone");
  });

  var qrLoginId = null, qrSource = null;
  $("qr-start").addEventListener("click", function () {
    api("/api/login/qr", { method: "POST" }).then(function (body) {
      qrLoginId = body.login_id;
      show("qr-box");
      $("qr-url").textContent = body.url;
      var img = $("qr-img");
      img.src = "/api/login/qr/" + qrLoginId + "/image?token=" + encodeURIComponent(token);
      img.classList.remove("hidden");
      say($("qr-status"), "waiting for scan…");
      if (qrSource) qrSource.close();
      qrSource = new EventSource("/api/login/qr/" + qrLoginId + "/wait?token=" + encodeURIComponent(token));
      qrSource.addEventListener("expired", function (ev) {
        var data = JSON.parse(ev.data);
        $("qr-url").textContent = data.url;
        img.src = "/api/login/qr/" + qrLoginId + "/image?token=" + encodeURIComponent(token);
        say($("qr-status"), "QR expired - a new one was generated, scan again.");
      });
      qrSource.addEventListener("needs_password", function () {
        var pw = window.prompt("Two-factor password:");
        if (pw === null) return;
        api("/api/login/qr/" + qrLoginId + "/password", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ password: pw }) })
          .then(function () { say($("qr-status"), "signed in"); show("step-complete"); setStep("complete"); })
          .catch(function (err) { showError(err.message); });
      });
      qrSource.addEventListener("success", function () {
        say($("qr-status"), "signed in"); show("step-complete"); setStep("complete");
        qrSource.close();
      });
      qrSource.addEventListener("error", function (ev) {
        try { var data = JSON.parse(ev.data); showError(data.detail || "QR login failed"); } catch (e) {}
        qrSource.close();
      });
    }).catch(function (err) { showError(err.message); });
  });

  $("complete-btn").addEventListener("click", function () {
    api("/api/setup/complete", { method: "POST" })
      .then(function (body) { say($("complete-result"), "setup complete - bot restarting (" + body.mode + ")…"); })
      .catch(function (err) { showError(err.message); });
  });

  var logEl = $("log");
  var logSource = new EventSource("/api/logs?token=" + encodeURIComponent(token));
  logSource.addEventListener("log", function (ev) {
    logEl.textContent += ev.data + "\n";
    logEl.scrollTop = logEl.scrollHeight;
  });
})();
</script>
</body>
</html>
"#;
