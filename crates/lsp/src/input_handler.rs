use std::str;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use collections::HashMap;
use futures::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt as _, SinkExt as _,
    channel::mpsc::{Receiver, Sender, channel},
};
use gpui::{BackgroundExecutor, Task};
use log::warn;
use parking_lot::Mutex;
use smol::io::BufReader;

use crate::{
    AnyResponse, CONTENT_LEN_HEADER, IoHandler, IoKind, NotificationOrRequest, RequestId,
    ResponseHandler,
};

const HEADER_DELIMITER: &[u8; 4] = b"\r\n\r\n";

/// Bounds messages buffered between the background reader and foreground dispatcher.
/// A full queue stops reading server stdout so the OS pipe applies backpressure.
const INCOMING_MESSAGE_QUEUE_CAPACITY: usize = 128;

/// Handler for stdout of language server.
pub struct LspStdoutHandler {
    pub(super) loop_handle: Task<Result<()>>,
    pub(super) incoming_messages: Receiver<NotificationOrRequest>,
}

async fn read_headers<Stdout>(reader: &mut BufReader<Stdout>, buffer: &mut Vec<u8>) -> Result<()>
where
    Stdout: AsyncRead + Unpin + Send + 'static,
{
    loop {
        if buffer.len() >= HEADER_DELIMITER.len()
            && buffer[(buffer.len() - HEADER_DELIMITER.len())..] == HEADER_DELIMITER[..]
        {
            return Ok(());
        }

        if reader.read_until(b'\n', buffer).await? == 0 {
            anyhow::bail!("cannot read LSP message headers");
        }
    }
}

impl LspStdoutHandler {
    pub fn new<Input>(
        stdout: Input,
        response_handlers: Arc<Mutex<Option<HashMap<RequestId, ResponseHandler>>>>,
        io_handlers: Arc<Mutex<HashMap<i32, IoHandler>>>,
        cx: BackgroundExecutor,
    ) -> Self
    where
        Input: AsyncRead + Unpin + Send + 'static,
    {
        let (tx, notifications_channel) = channel(INCOMING_MESSAGE_QUEUE_CAPACITY);
        let loop_handle = cx.spawn(Self::handler(stdout, tx, response_handlers, io_handlers));
        Self {
            loop_handle,
            incoming_messages: notifications_channel,
        }
    }

    async fn handler<Input>(
        stdout: Input,
        mut notifications_sender: Sender<NotificationOrRequest>,
        response_handlers: Arc<Mutex<Option<HashMap<RequestId, ResponseHandler>>>>,
        io_handlers: Arc<Mutex<HashMap<i32, IoHandler>>>,
    ) -> anyhow::Result<()>
    where
        Input: AsyncRead + Unpin + Send + 'static,
    {
        let mut stdout = BufReader::new(stdout);

        let mut buffer = Vec::new();

        loop {
            buffer.clear();

            read_headers(&mut stdout, &mut buffer).await?;

            let headers = std::str::from_utf8(&buffer)?;

            let message_len = headers
                .split('\n')
                .find(|line| line.starts_with(CONTENT_LEN_HEADER))
                .and_then(|line| line.strip_prefix(CONTENT_LEN_HEADER))
                .with_context(|| format!("invalid LSP message header {headers:?}"))?
                .trim_end()
                .parse()?;

            buffer.resize(message_len, 0);
            stdout.read_exact(&mut buffer).await?;

            if let Ok(message) = str::from_utf8(&buffer) {
                log::trace!("incoming message: {message}");
                for handler in io_handlers.lock().values_mut() {
                    handler(IoKind::StdOut, message);
                }
            }

            if let Ok(msg) = serde_json::from_slice::<NotificationOrRequest>(&buffer) {
                notifications_sender.send(msg).await?;
            } else if let Ok(AnyResponse {
                id, error, result, ..
            }) = serde_json::from_slice(&buffer)
            {
                let handler = {
                    response_handlers
                        .lock()
                        .as_mut()
                        .and_then(|handlers| handlers.remove(&id))
                };
                if let Some(handler) = handler {
                    if let Some(error) = error {
                        handler(Err(error)).await;
                    } else if let Some(result) = result {
                        handler(Ok(result.get().into())).await;
                    } else {
                        handler(Ok("null".into())).await;
                    }
                }
            } else {
                warn!(
                    "failed to deserialize LSP message:\n{}",
                    std::str::from_utf8(&buffer)?
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{AsyncWriteExt as _, FutureExt as _, StreamExt as _};
    use gpui::TestAppContext;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    fn framed_notification() -> String {
        let payload = r#"{"jsonrpc":"2.0","method":"test/notification","params":{}}"#;
        format!("Content-Length: {}\r\n\r\n{payload}", payload.len())
    }

    #[gpui::test]
    async fn incoming_notifications_apply_backpressure_without_losing_messages(
        cx: &mut TestAppContext,
    ) {
        const EXPECTED_QUEUE_CAPACITY: usize = 128;
        const TOTAL_MESSAGES: usize = EXPECTED_QUEUE_CAPACITY * 4;

        let (mut writer, reader) = async_pipe::pipe();
        let mut handler = LspStdoutHandler::new(
            reader,
            Arc::new(Mutex::new(Some(HashMap::default()))),
            Arc::new(Mutex::new(HashMap::default())),
            cx.background_executor.clone(),
        );
        let message = framed_notification();
        let writer_task = cx.background_executor.spawn(async move {
            for _ in 0..TOTAL_MESSAGES {
                writer.write_all(message.as_bytes()).await?;
            }
            anyhow::Ok(())
        });

        cx.run_until_parked();
        let mut received = 0;
        while handler.incoming_messages.try_recv().is_ok() {
            received += 1;
        }

        assert!(
            received <= EXPECTED_QUEUE_CAPACITY + 2,
            "expected a bounded queue near {EXPECTED_QUEUE_CAPACITY} messages, got {received}"
        );

        while received < TOTAL_MESSAGES {
            assert!(
                handler.incoming_messages.next().await.is_some(),
                "the message stream ended after {received} of {TOTAL_MESSAGES} notifications"
            );
            received += 1;
        }
        writer_task.await.expect("write all notifications");
    }

    #[gpui::test]
    async fn dropping_a_handler_releases_a_writer_blocked_by_backpressure(cx: &mut TestAppContext) {
        const TOTAL_MESSAGES: usize = 4096;

        let (mut writer, reader) = async_pipe::pipe();
        let handler = LspStdoutHandler::new(
            reader,
            Arc::new(Mutex::new(Some(HashMap::default()))),
            Arc::new(Mutex::new(HashMap::default())),
            cx.background_executor.clone(),
        );
        let writer_finished = Arc::new(AtomicBool::new(false));
        let writer_task = cx.background_executor.spawn({
            let writer_finished = writer_finished.clone();
            async move {
                let message = framed_notification();
                let result = async {
                    for _ in 0..TOTAL_MESSAGES {
                        writer.write_all(message.as_bytes()).await?;
                    }
                    anyhow::Ok(())
                }
                .await;
                writer_finished.store(true, Ordering::SeqCst);
                result
            }
        });

        cx.run_until_parked();
        assert!(
            !writer_finished.load(Ordering::SeqCst),
            "the writer should be blocked while the bounded queue is not consumed"
        );

        drop(handler);
        let writer_result = writer_task.fuse();
        let timeout = cx.background_executor.timer(Duration::from_secs(1)).fuse();
        futures::pin_mut!(writer_result, timeout);
        futures::select_biased! {
            result = writer_result => {
                assert!(result.is_err(), "closing a blocked reader should close its pipe");
            }
            () = timeout => panic!("blocked language-server writer did not stop after shutdown"),
        }
    }

    #[gpui::test]
    async fn test_read_headers() {
        let mut buf = Vec::new();
        let mut reader = smol::io::BufReader::new(b"Content-Length: 123\r\n\r\n" as &[u8]);
        read_headers(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf, b"Content-Length: 123\r\n\r\n");

        let mut buf = Vec::new();
        let mut reader = smol::io::BufReader::new(b"Content-Type: application/vscode-jsonrpc\r\nContent-Length: 1235\r\n\r\n{\"somecontent\":123}" as &[u8]);
        read_headers(&mut reader, &mut buf).await.unwrap();
        assert_eq!(
            buf,
            b"Content-Type: application/vscode-jsonrpc\r\nContent-Length: 1235\r\n\r\n"
        );

        let mut buf = Vec::new();
        let mut reader = smol::io::BufReader::new(b"Content-Length: 1235\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{\"somecontent\":true}" as &[u8]);
        read_headers(&mut reader, &mut buf).await.unwrap();
        assert_eq!(
            buf,
            b"Content-Length: 1235\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n"
        );
    }
}
