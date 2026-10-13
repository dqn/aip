use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, client};

const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Daemon {
    home: PathBuf,
    socket: PathBuf,
    codex: PathBuf,
}

impl Daemon {
    pub fn detect(home: &Path, codex: &Path) -> Result<Option<Self>> {
        let socket = home.join("app-server-control/app-server-control.sock");
        if !socket.try_exists()? {
            return Ok(None);
        }
        let daemon = Self {
            home: home.to_path_buf(),
            socket,
            codex: codex.to_path_buf(),
        };
        let version: Value = serde_json::from_slice(&daemon.run("version")?)
            .context("invalid Codex daemon status")?;
        if version["status"] != "running" {
            bail!("Codex control socket exists but no managed daemon is running");
        }
        let reported_socket = version["socketPath"]
            .as_str()
            .ok_or_else(|| anyhow!("Codex daemon status is missing socketPath"))?;
        if fs::canonicalize(reported_socket)? != fs::canonicalize(&daemon.socket)? {
            bail!("Codex daemon uses a different control socket");
        }
        daemon.connect()?;
        Ok(Some(daemon))
    }

    pub fn validate_auth(auth: &Value) -> Result<()> {
        expected_account(auth).map(|_| ())
    }

    pub fn restart_and_verify(&self, auth: &Value) -> Result<()> {
        let (account_id, email) = expected_account(auth)?;
        self.run("restart")?;
        let mut ws = self
            .connect()
            .context("could not connect to restarted Codex daemon")?;
        let account = rpc(&mut ws, 2, "account/read", json!({"refreshToken":false}))?;
        verify_account(&account, account_id, email.as_deref())
    }

    fn connect(&self) -> Result<WebSocket<UnixStream>> {
        let stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(RPC_TIMEOUT))?;
        stream.set_write_timeout(Some(RPC_TIMEOUT))?;
        let (mut ws, _) = client("ws://localhost/", stream)
            .map_err(|_| anyhow!("Codex daemon WebSocket handshake failed"))?;
        let initialized = rpc(
            &mut ws,
            1,
            "initialize",
            json!({
                "clientInfo":{"name":"aip","version":env!("CARGO_PKG_VERSION")},
                "capabilities":{"experimentalApi":true}
            }),
        )?;
        let reported_home = initialized["codexHome"]
            .as_str()
            .ok_or_else(|| anyhow!("Codex daemon did not report its home directory"))?;
        if fs::canonicalize(reported_home)? != fs::canonicalize(&self.home)? {
            bail!("Codex daemon uses a different home directory");
        }
        ws.send(Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))?;
        Ok(ws)
    }

    fn run(&self, action: &str) -> Result<Vec<u8>> {
        self.run_with_timeout(action, COMMAND_TIMEOUT)
    }

    fn run_with_timeout(&self, action: &str, timeout: Duration) -> Result<Vec<u8>> {
        // Capture stdout without risking a full pipe blocking the child. Never
        // include CLI stderr in errors because it may contain credential details.
        let output = tempfile::tempfile()?;
        let mut child = Command::new(&self.codex)
            .args(["app-server", "daemon", action])
            .env("CODEX_HOME", &self.home)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not run Codex daemon {action}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    bail!("Codex daemon {action} failed ({status})");
                }
                use std::io::{Read, Seek, SeekFrom};
                let mut output = output;
                output.seek(SeekFrom::Start(0))?;
                let mut bytes = Vec::new();
                output.read_to_end(&mut bytes)?;
                return Ok(bytes);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Codex daemon {action} timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn verify_account(account: &Value, account_id: &str, email: Option<&str>) -> Result<()> {
    if account["account"]["type"] != "chatgpt"
        || account["workspaceRouting"]["chatgptAccountId"].as_str() != Some(account_id)
        || email.is_some_and(|email| account["account"]["email"].as_str() != Some(email))
    {
        bail!("Codex daemon account does not match the selected profile");
    }
    Ok(())
}

fn expected_account(auth: &Value) -> Result<(&str, Option<String>)> {
    let account_id = auth["tokens"]["account_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("daemon verification requires a ChatGPT profile with account_id"))?;
    let email = if let Some(token) = auth["tokens"]["id_token"].as_str() {
        let payload = token
            .split('.')
            .nth(1)
            .ok_or_else(|| anyhow!("invalid profile ID token"))?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .map_err(|_| anyhow!("invalid profile ID token payload"))?;
        let claims: Value =
            serde_json::from_slice(&bytes).context("invalid profile ID token claims")?;
        claims["email"].as_str().map(str::to_owned)
    } else {
        None
    };
    Ok((account_id, email))
}

fn rpc(ws: &mut WebSocket<UnixStream>, id: u64, method: &str, params: Value) -> Result<Value> {
    ws.send(Message::Text(
        json!({"id":id,"method":method,"params":params})
            .to_string()
            .into(),
    ))?;
    let deadline = Instant::now() + RPC_TIMEOUT;
    loop {
        // Keep the timeout set at connection time. On macOS, resetting it
        // after the peer closes can fail even when a reply is still buffered.
        if Instant::now() >= deadline {
            bail!("Codex daemon {method} timed out");
        }
        let message = ws
            .read()
            .map_err(|_| anyhow!("could not read Codex daemon {method} response"))?;
        let Message::Text(text) = message else {
            continue;
        };
        let response: Value =
            serde_json::from_str(&text).context("invalid Codex daemon response")?;
        if response["id"].as_u64() != Some(id) {
            continue;
        }
        if response.get("error").is_some() {
            bail!("Codex daemon {method} request failed");
        }
        return response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("Codex daemon {method} response is missing result"));
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    use std::thread;
    use tungstenite::{Message, accept};

    pub(crate) fn fixture(stale: bool) -> (tempfile::TempDir, PathBuf, thread::JoinHandle<()>) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let socket = home.join("app-server-control/app-server-control.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let codex = home.join("codex");
        let version = json!({"status":"running", "socketPath": socket});
        fs::write(home.join("version.json"), version.to_string()).unwrap();
        fs::write(&codex, format!(
            "#!/bin/sh\ntest \"$CODEX_HOME\" = '{}' || exit 4\ncase \"$3\" in\nversion) cat '{}/version.json' ;;\nrestart) cat '{}/auth.json' > '{}/restarted-auth.json' || exit 2; touch '{}/restarted' ;;\n*) exit 3 ;;\nesac\n",
            home.display(), home.display(), home.display(), home.display(), home.display()
        )).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        let root = home.to_path_buf();
        let handle = thread::spawn(move || {
            // One connection before switching, one after restarting.
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut ws = accept(stream).unwrap();
                let init: Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                ws.send(Message::Text(
                    json!({"id":init["id"],"result":{"codexHome":root}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
                let _: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                let Ok(message) = ws.read() else { continue };
                let req: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                assert_eq!(req["method"], "account/read");
                assert_eq!(req["params"]["refreshToken"], false);
                let id = if !stale && root.join("restarted").exists() {
                    let captured: Value = serde_json::from_slice(
                        &fs::read(root.join("restarted-auth.json")).unwrap(),
                    )
                    .unwrap();
                    captured["tokens"]["account_id"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                } else {
                    "old".to_owned()
                };
                ws.send(Message::Text(
                    json!({"method":"account/updated","params":{}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
                ws.send(Message::Text(json!({"id":req["id"],"result":{"account":{"type":"chatgpt","email":"user@example.invalid","planType":"plus"},"workspaceRouting":{"chatgptAccountId":id}}}).to_string().into())).unwrap();
            }
        });
        (dir, codex, handle)
    }

    #[test]
    fn absent_daemon_does_not_require_codex() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            Daemon::detect(home.path(), Path::new("/nonexistent/codex"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn running_daemon_is_restarted_and_verified() {
        let (home, codex, server) = fixture(false);
        let daemon = Daemon::detect(home.path(), &codex)
            .unwrap()
            .expect("running daemon must be detected");
        fs::write(home.path().join("auth.json"), expected_auth().to_string()).unwrap();
        daemon.restart_and_verify(&expected_auth()).unwrap();
        assert!(home.path().join("restarted").exists());
        server.join().unwrap();
    }

    #[test]
    fn stale_account_after_restart_is_rejected() {
        let (home, codex, server) = fixture(true);
        let daemon = Daemon::detect(home.path(), &codex)
            .unwrap()
            .expect("running daemon must be detected");
        fs::write(home.path().join("auth.json"), expected_auth().to_string()).unwrap();
        assert!(daemon.restart_and_verify(&expected_auth()).is_err());
        server.join().unwrap();
    }

    pub(crate) fn expected_auth() -> Value {
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"email":"user@example.invalid"}"#);
        json!({"tokens":{"account_id":"new","id_token":format!("header.{payload}.signature")}})
    }

    #[test]
    fn account_verification_accepts_runtime_shape_and_rejects_counterexamples() {
        // Captured from Codex 0.162.1 account/read with experimentalApi enabled.
        // Only the email and account ID have been anonymized.
        let response = json!({"account":{"type":"chatgpt","email":"user@example.invalid","planType":"prolite"},"requiresOpenaiAuth":true,"workspaceRouting":{"chatgptAccountId":"new","backendOrigin":"https://chatgpt.com","accountRoutingOverride":"NO_CONSTRAINT"}});
        assert!(verify_account(&response, "new", Some("user@example.invalid")).is_ok());
        assert!(verify_account(&response, "old", Some("user@example.invalid")).is_err());
        assert!(verify_account(&response, "new", Some("another@example.invalid")).is_err());
        for invalid in [
            Value::Null,
            json!({}),
            json!({"account":response["account"]}),
            json!({"account":"malformed","workspaceRouting":response["workspaceRouting"]}),
        ] {
            assert!(verify_account(&invalid, "new", Some("user@example.invalid")).is_err());
        }
        let mut without_email = response.clone();
        without_email["account"]["email"] = Value::Null;
        assert!(verify_account(&without_email, "new", None).is_ok());
    }

    #[test]
    fn invalid_profile_metadata_is_rejected_before_restart() {
        for auth in [
            json!({}),
            json!({"OPENAI_API_KEY":"sentinel"}),
            json!({"tokens":{"account_id":""}}),
            json!({"tokens":{"account_id":"new","id_token":"invalid"}}),
        ] {
            assert!(Daemon::validate_auth(&auth).is_err());
        }
    }

    #[test]
    fn command_failure_does_not_leak_stderr() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join("codex");
        fs::write(&codex, "#!/bin/sh\necho secret-sentinel >&2\nexit 7\n").unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        let daemon = Daemon {
            home: home.path().to_path_buf(),
            socket: home.path().join("unused"),
            codex,
        };
        let error = format!("{:#}", daemon.run("restart").unwrap_err());
        assert!(error.contains("7"));
        assert!(!error.contains("secret-sentinel"));
    }

    #[test]
    fn command_timeout_is_reported() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join("codex");
        fs::write(&codex, "#!/bin/sh\nexec sleep 5\n").unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        let daemon = Daemon {
            home: home.path().to_path_buf(),
            socket: home.path().join("unused"),
            codex,
        };
        let started = Instant::now();
        assert!(
            daemon
                .run_with_timeout("restart", Duration::from_millis(25))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn rpc_reads_buffered_response_after_peer_disconnect() {
        use tungstenite::protocol::Role;
        let (stream, peer) = UnixStream::pair().unwrap();
        stream.set_read_timeout(Some(RPC_TIMEOUT)).unwrap();
        let server = thread::spawn(move || {
            let mut ws = WebSocket::from_raw_socket(peer, Role::Server, None);
            let request: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            // Make notification decoding take longer than the peer's final send
            // so the reply remains buffered after the server disconnects.
            ws.send(Message::Text(
                json!({"method":"notice","params":{"padding":"x".repeat(1_000_000)}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
            ws.send(Message::Text(
                json!({"id":request["id"],"result":{"sentinel":"buffered-reply"}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        });
        let mut ws = WebSocket::from_raw_socket(stream, Role::Client, None);
        assert_eq!(
            rpc(&mut ws, 2, "account/read", json!({})).unwrap()["sentinel"],
            "buffered-reply"
        );
        server.join().unwrap();
    }

    #[test]
    fn missing_malformed_and_unmanaged_status_are_rejected() {
        let home = tempfile::tempdir().unwrap();
        let socket = home
            .path()
            .join("app-server-control/app-server-control.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let _listener = UnixListener::bind(socket).unwrap();
        let codex = home.path().join("codex");
        for response in [
            "{}",
            "malformed",
            r#"{"status":"running"}"#,
            r#"{"status":"stopped"}"#,
        ] {
            fs::write(&codex, format!("#!/bin/sh\nprintf '%s' '{response}'\n")).unwrap();
            fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(Daemon::detect(home.path(), &codex).is_err());
        }
    }
}
