//! Slopty's push relay: a Cloudflare Worker that carries a sealed note from a Slopty server to
//! APNs (`docs/decisions/platform.md`, "Notes reach a pocketed phone").
//!
//! It has one route, `POST /push`. A server signs each push with its install key; the relay
//! checks the signature and its time, binds the phone's token to the first installs that push to
//! it, holds each phone and each install to a rate, and sends the push on to APNs under the
//! team's key with its own fixed words. It reads nothing: the note is sealed to the phone, and
//! it is never logged. Its checks are `slopty_push::relay`'s, tested on the host; this crate is
//! the Worker around them, built for `wasm32-unknown-unknown` (`worker-build`), with the
//! binding's record ([`binding`]) the one piece of its own.
//!
//! It keeps nothing but the bindings, in KV, and the rate limiters' counts. The person deploys
//! it with `wrangler deploy` after `wrangler secret put` for `APNS_KEY` (the `.p8`'s text),
//! `APNS_KEY_ID` and `APNS_TEAM_ID`; `wrangler.toml` names the KV namespace, the limiters and
//! the app's bundle identifier.

pub mod binding;

#[cfg(target_arch = "wasm32")]
mod worker;
