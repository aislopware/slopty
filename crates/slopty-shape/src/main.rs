//! `slopty-shape` — a UDP relay that degrades the link between a Slopty client and a worker.
//!
//! The two ends speak QUIC to each other through this process, so the impairment lands below
//! the congestion controller and it reacts to the queue, the loss and the delay the way it
//! would to a real bottleneck. Nothing here needs a password, which is the whole point: the
//! kernel's shapers do, and the rulings that wait on a collapsed link have waited long enough.
//!
//! Point it at the worker's UDP address and connect the client to *this* address. Plain QUIC
//! never looks for another path, so the relay stays the only way across for the whole run.
//!
//! ```text
//! slopty-shape --to 192.168.1.10:45560 --delay 60ms --jitter 10ms --rate 600kB --loss 1%
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_shape::relay::{Carried, Relay};
use slopty_shape::{Link, Tally};

/// How often the run's tally is printed.
const REPORT: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(about = "Relay UDP between a Slopty client and worker over a link you can degrade")]
struct Opts {
    /// UDP address to listen on. The client dials this, with `0.0.0.0` read as loopback: the
    /// relay must be able to answer a client there and reach the worker on a real interface, and
    /// one socket bound to `127.0.0.1` cannot do both.
    #[arg(long, default_value = "0.0.0.0:45570")]
    at: SocketAddr,
    /// The worker's UDP address, where packets from the client go.
    #[arg(long)]
    to: SocketAddr,
    /// One-way delay each direction, e.g. `60ms`.
    #[arg(long, default_value = "0ms", value_parser = duration)]
    delay: Duration,
    /// Extra delay spread over `0..jitter`, e.g. `10ms`.
    #[arg(long, default_value = "0ms", value_parser = duration)]
    jitter: Duration,
    /// Bytes per second each direction, e.g. `600kB`. Zero for no limit.
    #[arg(long, default_value = "0", value_parser = rate)]
    rate: u64,
    /// Bytes that may queue for the rate limit. Defaults to a quarter second of it.
    #[arg(long)]
    queue: Option<u64>,
    /// Share of packets dropped, e.g. `1%` or `0.01`.
    #[arg(long, default_value = "0", value_parser = share)]
    loss: f32,
    /// Seed for loss and jitter, so a run repeats.
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

/// `60ms`, `2s`, or a bare number of milliseconds.
fn duration(text: &str) -> Result<Duration, String> {
    let parse = |digits: &str| digits.trim().parse::<u64>().map_err(|e| e.to_string());
    if let Some(digits) = text.strip_suffix("ms") {
        parse(digits).map(Duration::from_millis)
    } else if let Some(digits) = text.strip_suffix('s') {
        parse(digits).map(Duration::from_secs)
    } else {
        parse(text).map(Duration::from_millis)
    }
}

/// `600kB`, `2MB`, or a bare number of bytes per second.
fn rate(text: &str) -> Result<u64, String> {
    let parse = |digits: &str| digits.trim().parse::<u64>().map_err(|e| e.to_string());
    let scaled = |digits: &str, by: u64| {
        parse(digits).and_then(|n| n.checked_mul(by).ok_or_else(|| "rate too large".to_owned()))
    };
    if let Some(digits) = text.strip_suffix("MB") {
        scaled(digits, 1_000_000)
    } else if let Some(digits) = text.strip_suffix("kB") {
        scaled(digits, 1_000)
    } else {
        parse(text)
    }
}

/// `1%` or `0.01`.
fn share(text: &str) -> Result<f32, String> {
    let (digits, scale) =
        text.strip_suffix('%').map_or((text, 1.0), |digits| (digits, 1.0 / 100.0));
    let value: f32 = digits.trim().parse().map_err(|_bad| format!("not a number: {text}"))?;
    let value = value * scale;
    if (0.0..=1.0).contains(&value) { Ok(value) } else { Err(format!("not a share: {text}")) }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let opts = Opts::parse();
    let link = Link {
        delay: opts.delay,
        jitter: opts.jitter,
        loss: opts.loss,
        rate: opts.rate,
        // A quarter second of the rate is the shape of a home router's buffer: enough to ride
        // out a burst, little enough that standing in it is felt.
        queue: opts.queue.unwrap_or(opts.rate / 4),
    };
    let relay = Arc::new(
        Relay::bind(opts.at, opts.to, link, opts.seed)
            .await
            .with_context(|| format!("bind {}", opts.at))?,
    );
    tracing::info!(addr = %relay.addr()?, worker = %opts.to, ?link, "shaping");
    tokio::spawn(report(Arc::clone(&relay)));
    relay.run().await.context("relay")
}

/// Print what each direction has carried, so a run's numbers can be read off the log. Runs for
/// as long as the relay does.
#[expect(clippy::infinite_loop, reason = "a reporting task ends when the process does")]
async fn report(relay: Arc<Relay>) {
    let mut ticker = tokio::time::interval(REPORT);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let Carried { up, down } = relay.carried().await;
        tracing::info!(up = %line(up), down = %line(down), "carried");
    }
}

/// One direction's tally, as one readable field.
fn line(tally: Tally) -> String {
    let Tally { sent, lost, overflowed, bytes } = tally;
    format!("{sent} sent, {lost} lost, {overflowed} overflowed, {} kB", bytes / 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_read_in_the_units_they_are_written() {
        assert_eq!(duration("60ms"), Ok(Duration::from_millis(60)));
        assert_eq!(duration("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(duration("40"), Ok(Duration::from_millis(40)), "a bare number is ms");
        duration("soon").unwrap_err();
    }

    #[test]
    fn rates_are_read_in_the_units_they_are_written() {
        assert_eq!(rate("600kB"), Ok(600_000));
        assert_eq!(rate("2MB"), Ok(2_000_000));
        assert_eq!(rate("1500"), Ok(1_500));
        rate("fast").unwrap_err();
    }

    #[test]
    fn a_share_is_a_percentage_or_a_fraction_and_never_more_than_all() {
        assert_eq!(share("1%"), Ok(0.01));
        assert_eq!(share("0.05"), Ok(0.05));
        assert_eq!(share("0"), Ok(0.0));
        share("120%").unwrap_err();
        share("-1").unwrap_err();
    }
}
