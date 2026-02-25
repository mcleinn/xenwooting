use anyhow::Result;
use std::thread;
use std::time::Duration;

fn main() -> Result<()> {
    env_logger::init();

    // Set XENWOOTING_MTS_REINIT=1 if you want to force MTS_Reinitialize() first.
    let reinit = std::env::var("XENWOOTING_MTS_REINIT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let master = xenwooting::mts::MtsMaster::register(reinit)?;
    master.set_scale_name("xenwooting-demo")?;
    master.enable_all_channels();

    // A quick sanity tuning: make channel 0 note 60 = 440Hz.
    master.set_multi_channel_note_tuning(0, 60, 440.0);
    // And channel 1 note 60 an octave above.
    master.set_multi_channel_note_tuning(1, 60, 880.0);

    println!("mts_demo: registered master; sleeping 10s");
    thread::sleep(Duration::from_secs(10));
    println!("mts_demo: exiting (Drop will deregister)");
    Ok(())
}
