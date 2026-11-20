//! **Mocked.** The two wallets' balances, and converting between satoshis and Paykit's
//! `btc` amounts.
//!
//! Paykit does not hold money: it tells a payment rail to pay, and records what happened. In
//! this demo the rail is the SDK's `FakeRail`, which pays nothing real, and these balances are
//! plain numbers kept in WebAssembly memory. After a payment, the engine moves them by exactly
//! the amount `FakeRail` reports it paid (`FakeRail::paid_amount`), so they follow what Paykit
//! asked the rail to do, but no money, chain or key of value is involved.

use paykit_sdk::{Asset, Decimal, PaymentAmount};

use crate::error::Failure;

/// Satoshis in one bitcoin.
const SATS_PER_BTC: u64 = 100_000_000;
/// Digits after the decimal point in a bitcoin amount that is a whole number of satoshis.
const BTC_DECIMALS: usize = 8;
/// The most satoshis that can ever exist: 21 million bitcoin.
pub const MAX_SATS: u64 = 21_000_000 * SATS_PER_BTC;
/// Alice's mock balance when a world starts.
const ALICE_START_SATS: u64 = 100_000;

/// The mock balances of Alice (who pays) and Bob (who gets paid), in satoshis.
#[derive(Debug)]
pub struct Ledger {
    /// Alice's balance.
    pub alice_sats: u64,
    /// Bob's balance.
    pub bob_sats: u64,
}

impl Ledger {
    /// A new world's balances: Alice holds 100,000 sats, Bob none.
    pub fn new() -> Self {
        Self {
            alice_sats: ALICE_START_SATS,
            bob_sats: 0,
        }
    }

    /// Moves `sats` from Alice to Bob.
    ///
    /// # Errors
    ///
    /// When Alice holds less than `sats`. The engine checks before it pays, so this means the
    /// rail paid more than was asked.
    pub fn move_alice_to_bob(&mut self, sats: u64) -> Result<(), Failure> {
        // `checked_sub` returns `None` instead of wrapping around below zero. Plain `-` on
        // integers panics on overflow in debug builds and wraps in release builds, and neither
        // is acceptable for a balance.
        self.alice_sats = self
            .alice_sats
            .checked_sub(sats)
            .ok_or_else(|| Failure::new("the mock rail paid more than Alice holds"))?;
        self.bob_sats = self
            .bob_sats
            .checked_add(sats)
            .ok_or_else(|| Failure::new("Bob's mock balance overflowed"))?;
        Ok(())
    }
}

/// The satoshi amount `value`, as a JavaScript number gives it, checked.
///
/// # Errors
///
/// When `value` is not a whole number from 1 to [`MAX_SATS`].
pub fn sats_from_number(value: f64) -> Result<u64, Failure> {
    // A JavaScript number is an `f64`. Only after these checks is the cast exact: the value is
    // a whole number, and 2.1e15 is well below 2^53, where `f64` stops holding every integer.
    if !value.is_finite() || value.fract() != 0.0 || value < 1.0 || value > MAX_SATS as f64 {
        return Err(Failure::new(
            "the amount must be a whole number of sats, from 1 to 2,100,000,000,000,000",
        ));
    }
    Ok(value as u64)
}

/// `sats` as a Paykit amount in `btc`.
///
/// A Paykit amount is a decimal string in whole units of its asset (Paykit wire 1.2), so
/// 1,000 sats is `"0.00001"` btc. The conversion uses integers only: floating point cannot
/// hold most decimal fractions exactly.
pub fn btc_amount(sats: u64) -> Result<PaymentAmount, Failure> {
    let whole = sats / SATS_PER_BTC;
    let fraction = sats % SATS_PER_BTC;
    let text = if fraction == 0 {
        whole.to_string()
    } else {
        // `{:08}` pads the fraction with leading zeros to eight digits; the trailing zeros
        // are then trimmed, as they say nothing.
        let digits = format!("{fraction:0width$}", width = BTC_DECIMALS);
        format!("{whole}.{}", digits.trim_end_matches('0'))
    };
    Ok(PaymentAmount {
        value: Decimal::new(&text)
            .map_err(|_| Failure::new("the amount is not a valid decimal"))?,
        asset: Asset::new("btc").map_err(|_| Failure::new("btc is not a valid asset"))?,
    })
}

/// A Paykit amount in `btc` as satoshis.
///
/// # Errors
///
/// When the asset is not `btc`, or the amount is not a whole number of satoshis.
pub fn sats_from_btc(amount: &PaymentAmount) -> Result<u64, Failure> {
    let refused = || Failure::new("the amount is not a whole number of sats in btc");
    if amount.asset.as_str() != "btc" {
        return Err(refused());
    }
    let (whole, fraction) = match amount.value.as_str().split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (amount.value.as_str(), ""),
    };
    if fraction.len() > BTC_DECIMALS {
        return Err(refused());
    }
    let whole: u64 = whole.parse().map_err(|_| refused())?;
    // Pads "00001" to "00001000": eight digits, so it reads as satoshis.
    let fraction: u64 = format!("{fraction:0<width$}", width = BTC_DECIMALS)
        .parse()
        .map_err(|_| refused())?;
    whole
        .checked_mul(SATS_PER_BTC)
        .and_then(|sats| sats.checked_add(fraction))
        .ok_or_else(refused)
}
