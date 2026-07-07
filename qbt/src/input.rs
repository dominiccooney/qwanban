use std::time::Duration;
use crate::pal;

pub(crate) async fn send_input_demo() -> anyhow::Result<()> {
    for ch in "dir\r".chars().into_iter() {
        let key = pal::key_for_character(ch)?;
        pal::send_key_down(key)?;
        tokio::time::sleep(Duration::from_millis(60)).await;
        pal::send_key_up(key)?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    Ok(())
}