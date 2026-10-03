use super::{Endpoint, Identity, managed};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};

const REQUEST_LIMIT: usize = 8192;
const RESPONSE_LIMIT: usize = 32 * 1_048_576;
async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Err(std::io::Error::other("incomplete control frame"));
        }
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |p| p + 1);
        if bytes.len() + n > limit {
            return Err(std::io::Error::other("oversized control frame"));
        }
        bytes.extend_from_slice(&buf[..n]);
        reader.consume(n);
        if bytes.last() == Some(&b'\n') {
            return Ok(bytes);
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Ping,
    Status { include_values: bool },
    Refresh { workspace: Option<String> },
    Reload,
    RunJob { name: Option<String> },
    Stop,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub enum ProtocolError {
    #[serde(rename = "unknown command")]
    UnknownCommand,
    #[serde(rename = "invalid protocol/endpoint")]
    InvalidProtocol,
    #[serde(rename = "busy")]
    Busy,
}
#[derive(Serialize)]
struct ErrorResult {
    error: ProtocolError,
}
impl Response {
    fn error(identity: Identity, error: ProtocolError) -> Self {
        Self {
            version: 1,
            ok: false,
            ready: true,
            identity,
            result: serde_json::to_value(ErrorResult { error }).expect("error serialization"),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub endpoint: String,
    pub command: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub include_values: bool,
    #[serde(default)]
    pub job: Option<String>,
}
impl Request {
    /// Version 1 historically ignores fields irrelevant to a command; retain that policy.
    pub fn decode(&self) -> std::result::Result<Command, ProtocolError> {
        if self.version != 1 {
            return Err(ProtocolError::InvalidProtocol);
        }
        Ok(match self.command.as_str() {
            "ping" => Command::Ping,
            "status" => Command::Status {
                include_values: self.include_values,
            },
            "refresh" => Command::Refresh {
                workspace: self.workspace.clone(),
            },
            "reload" => Command::Reload,
            "run-job" => Command::RunJob {
                name: self.job.clone(),
            },
            "stop" => Command::Stop,
            _ => return Err(ProtocolError::UnknownCommand),
        })
    }
    fn encode(endpoint: String, command: Command) -> Self {
        let mut request = Self {
            version: 1,
            endpoint,
            command: String::new(),
            workspace: None,
            include_values: false,
            job: None,
        };
        request.command = match command {
            Command::Ping => "ping",
            Command::Stop => "stop",
            Command::Reload => "reload",
            Command::Status { include_values } => {
                request.include_values = include_values;
                "status"
            }
            Command::Refresh { workspace } => {
                request.workspace = workspace;
                "refresh"
            }
            Command::RunJob { name } => {
                request.job = name;
                "run-job"
            }
        }
        .into();
        request
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub ok: bool,
    pub identity: Identity,
    pub ready: bool,
    pub result: serde_json::Value,
}
pub struct Control {
    pub command: Command,
    pub reply: oneshot::Sender<Response>,
}
pub async fn serve(
    listener: UnixListener,
    tx: mpsc::Sender<Control>,
    identity: Identity,
    cancel: tokio_util::sync::CancellationToken,
) {
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((mut stream,_)) = accepted else { break; };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    // Wait briefly for write readiness without spawning an unbounded
                    // rejection task. try_write alone can lose the busy response.
                    let response = Response::error(identity.clone(), ProtocolError::Busy);
                    if let Ok(mut bytes) = serde_json::to_vec(&response) {
                        bytes.push(b'\n');
                        let _ = tokio::time::timeout(Duration::from_millis(50), stream.write_all(&bytes)).await;
                    }
                    continue;
                };
                let tx = tx.clone(); let identity = identity.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let _ = tokio::time::timeout(Duration::from_secs(5), async {
                        let mut reader = BufReader::new(&mut stream);
                        let bytes = read_frame(&mut reader, REQUEST_LIMIT).await?;
                        let req = serde_json::from_slice::<Request>(&bytes);
                        let response = match req {
                            Ok(request) if request.endpoint == identity.endpoint => match request.decode() {
                                Ok(command) => {
                                    let (reply, rx) = oneshot::channel();
                                    if tx.try_send(Control { command, reply }).is_ok() { rx.await.map_err(|_| std::io::Error::other("runner stopped"))? }
                                    else { Response::error(identity.clone(), ProtocolError::Busy) }
                                }
                                Err(error) => Response::error(identity.clone(), error),
                            },
                            _ => Response::error(identity.clone(), ProtocolError::InvalidProtocol),
                        };
                        let mut bytes = serde_json::to_vec(&response)?; bytes.push(b'\n'); stream.write_all(&bytes).await
                    }).await;
                });
            }
        }
    }
    // Let the stop acknowledgement reach its caller before closing control.
    let _ = tokio::time::timeout(Duration::from_millis(100), async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    // Connections contain only IPC, never process supervision; aborting is safe here.
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}
pub async fn request(endpoint: &Endpoint, command: Command) -> Result<Response> {
    managed(&endpoint.control, true)?;
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = UnixStream::connect(&endpoint.control)
            .await
            .context("runner unavailable")?;
        let req = Request::encode(endpoint.hash.clone(), command);
        let mut bytes = serde_json::to_vec(&req)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= REQUEST_LIMIT, "control request too large");
        // A saturated server may send busy and close before reading the request.
        // Read that bounded response even if our write raced with its close.
        let _ = stream.write_all(&bytes).await;
        let mut reader = BufReader::new(stream);
        let bytes = read_frame(&mut reader, RESPONSE_LIMIT).await?;
        let response: Response = serde_json::from_slice(&bytes)?;
        ensure!(
            response.version == 1
                && response.identity.endpoint == endpoint.hash
                && response.identity.socket == endpoint.socket,
            "control identity mismatch"
        );
        Ok::<_, anyhow::Error>(response)
    })
    .await
    .context("control deadline exceeded")??;
    Ok(response)
}
pub async fn matching(endpoint: &Endpoint, identity: &Identity) -> Result<()> {
    let r = request(endpoint, Command::Ping).await?;
    ensure!(
        &r.identity == identity,
        "runner conflict: endpoint uses different config/state locations"
    );
    ensure!(r.ok && r.ready, "runner is not ready");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn frames_support_partial_reads_exact_limits_eof_and_overflow() {
        let (mut writer, reader) = tokio::io::duplex(2);
        let task = tokio::spawn(async move {
            writer.write_all(b"ab").await.unwrap();
            tokio::task::yield_now().await;
            writer.write_all(b"c\nnext\n").await.unwrap();
        });
        let mut reader = BufReader::new(reader);
        assert_eq!(read_frame(&mut reader, 4).await.unwrap(), b"abc\n");
        assert_eq!(read_frame(&mut reader, 5).await.unwrap(), b"next\n");
        task.await.unwrap();
        assert!(read_frame(&mut reader, 5).await.is_err());
        for bytes in [b"abcde\n".as_slice(), b"abcd", b""] {
            assert!(read_frame(&mut BufReader::new(bytes), 4).await.is_err());
        }
    }
    #[test]
    fn version_one_decodes_commands_and_ignores_irrelevant_fields() {
        for command in [
            Command::Ping,
            Command::Stop,
            Command::Reload,
            Command::Status {
                include_values: true,
            },
            Command::Refresh {
                workspace: Some("w".into()),
            },
            Command::RunJob {
                name: Some("job".into()),
            },
        ] {
            let req = Request::encode("endpoint".into(), command.clone());
            let encoded = serde_json::to_vec(&req).unwrap();
            assert_eq!(
                serde_json::from_slice::<Request>(&encoded)
                    .unwrap()
                    .decode()
                    .unwrap(),
                command
            );
        }
        let mut req = Request::encode("endpoint".into(), Command::Ping);
        req.workspace = Some("ignored".into());
        req.job = Some("ignored".into());
        req.include_values = true;
        assert_eq!(req.decode().unwrap(), Command::Ping);
        req.command = "future".into();
        assert!(matches!(req.decode(), Err(ProtocolError::UnknownCommand)));
        req.version = 2;
        assert!(matches!(req.decode(), Err(ProtocolError::InvalidProtocol)));
    }
}
