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

    let (start_x, start_y, end_x, end_y) = (10.0, 400.0, 300.0, 200.0);

    for i in 0..100 {
        let t = i as f32 / 100.0;
        let x = start_x + t * (end_x - start_x);
        let y = start_y + t * (end_y - start_y);
        pal::mouse_move_to((x as i32, y as i32)).await?;
    }

    Ok(())
}