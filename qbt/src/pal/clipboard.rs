use std::sync::{Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;

enum Command {
    Get(mpsc::Sender<anyhow::Result<Option<String>>>),
    Set(String, mpsc::Sender<anyhow::Result<()>>),
    Shutdown,
}

struct Worker {
    sender: mpsc::Sender<Command>,
    handle: JoinHandle<()>,
}

fn worker() -> &'static Mutex<Option<Worker>> {
    static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();
    WORKER.get_or_init(|| Mutex::new(None))
}

fn command_sender() -> anyhow::Result<mpsc::Sender<Command>> {
    let mut worker = worker().lock().unwrap();
    if worker.is_none() {
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("qbt-clipboard".into())
            .spawn(move || serve(receiver))
            .map_err(|error| anyhow::anyhow!("failed to start clipboard thread: {error}"))?;
        *worker = Some(Worker { sender, handle });
    }
    Ok(worker.as_ref().unwrap().sender.clone())
}

fn serve(receiver: mpsc::Receiver<Command>) {
    let mut clipboard = None;
    for command in receiver {
        match command {
            Command::Get(reply) => {
                let result = match get_or_initialize(&mut clipboard) {
                    Ok(clipboard) => match clipboard.get_text() {
                        Ok(text) => Ok(Some(text)),
                        Err(arboard::Error::ContentNotAvailable) => Ok(None),
                        Err(error) => Err(anyhow::anyhow!(error)),
                    },
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            Command::Set(text, reply) => {
                let result = match get_or_initialize(&mut clipboard) {
                    Ok(clipboard) => clipboard.set_text(text).map_err(anyhow::Error::from),
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            Command::Shutdown => break,
        }
    }
}

fn get_or_initialize(
    clipboard: &mut Option<arboard::Clipboard>,
) -> anyhow::Result<&mut arboard::Clipboard> {
    if clipboard.is_none() {
        *clipboard = Some(arboard::Clipboard::new()?);
    }
    Ok(clipboard.as_mut().unwrap())
}

pub(crate) fn get_text() -> anyhow::Result<Option<String>> {
    let (sender, receiver) = mpsc::channel();
    command_sender()?
        .send(Command::Get(sender))
        .map_err(|_| anyhow::anyhow!("clipboard thread stopped"))?;
    receiver
        .recv()
        .map_err(|_| anyhow::anyhow!("clipboard thread stopped"))?
}

pub(crate) fn set_text(text: String) -> anyhow::Result<()> {
    let (sender, receiver) = mpsc::channel();
    command_sender()?
        .send(Command::Set(text, sender))
        .map_err(|_| anyhow::anyhow!("clipboard thread stopped"))?;
    receiver
        .recv()
        .map_err(|_| anyhow::anyhow!("clipboard thread stopped"))?
}

pub(crate) fn shutdown() -> anyhow::Result<()> {
    let Some(worker) = worker().lock().unwrap().take() else {
        return Ok(());
    };
    let _ = worker.sender.send(Command::Shutdown);
    worker
        .handle
        .join()
        .map_err(|_| anyhow::anyhow!("clipboard thread panicked"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_can_restart_after_clean_shutdown() {
        command_sender().unwrap();
        shutdown().unwrap();
        command_sender().unwrap();
        shutdown().unwrap();
    }
}
