//! Application-side budget enforcement for AI calls.
//!
//! Before every call the caller reserves the call's worst case (prompt plus
//! the `max_tokens` it allows). Reservations count against the budget until
//! the call settles with its actual usage, so concurrent in-flight calls can
//! never jointly exceed the limit. These are protective estimates, not
//! provider-side billing caps.

use serde::{Deserialize, Serialize};

use crate::pricing::Micros;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_tokens: Option<u64>,
    pub max_cost: Option<Micros>,
    pub max_calls: Option<u32>,
    /// The user accepted running with unknown pricing. Without this, calls
    /// whose cost cannot be computed are refused whenever a cost limit is set.
    pub allow_unknown_cost: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("token budget would be exceeded ({needed} needed, {remaining} remaining)")]
    Tokens { needed: u64, remaining: u64 },
    #[error("cost budget would be exceeded")]
    Cost { needed: Micros, remaining: Micros },
    #[error("call limit reached")]
    Calls,
    #[error("cost of this call is unknown and unknown pricing was not accepted")]
    UnknownCost,
    #[error("budget is paused")]
    Paused,
}

/// Proof of a reservation; must be settled exactly once.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a reservation must be settled"]
pub struct Reservation {
    id: u64,
    tokens: u64,
    cost: Option<Micros>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub tokens: u64,
    /// Known cost. `cost_unknown` is set if any spend could not be priced.
    pub cost: Micros,
    pub cost_unknown: bool,
    pub calls: u32,
}

#[derive(Debug)]
pub struct Guard {
    limits: Limits,
    spent: Totals,
    reserved_tokens: u64,
    reserved_cost: Micros,
    in_flight: u32,
    next_id: u64,
    paused: bool,
}

impl Guard {
    /// `already_spent` carries usage counted against the same limits, such
    /// as earlier spend today for a daily budget.
    pub fn new(limits: Limits, already_spent: Totals) -> Self {
        Self {
            limits,
            spent: already_spent,
            reserved_tokens: 0,
            reserved_cost: 0,
            in_flight: 0,
            next_id: 0,
            paused: false,
        }
    }

    pub fn spent(&self) -> &Totals {
        &self.spent
    }

    pub fn in_flight(&self) -> u32 {
        self.in_flight
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Reserves a call's worst case. `worst_cost` is `None` if unpriced.
    pub fn reserve(&mut self, worst_tokens: u64, worst_cost: Option<Micros>) -> Result<Reservation, Refusal> {
        if self.paused {
            return Err(Refusal::Paused);
        }
        if let Some(max) = self.limits.max_calls
            && self.spent.calls + self.in_flight >= max
        {
            return Err(Refusal::Calls);
        }
        if let Some(max) = self.limits.max_tokens {
            let remaining = max.saturating_sub(self.spent.tokens + self.reserved_tokens);
            if worst_tokens > remaining {
                return Err(Refusal::Tokens { needed: worst_tokens, remaining });
            }
        }
        if let Some(max) = self.limits.max_cost {
            match worst_cost {
                None if !self.limits.allow_unknown_cost => return Err(Refusal::UnknownCost),
                None => {}
                Some(c) => {
                    let remaining = max.saturating_sub(self.spent.cost + self.reserved_cost);
                    if c > remaining {
                        return Err(Refusal::Cost { needed: c, remaining });
                    }
                }
            }
        }
        self.reserved_tokens += worst_tokens;
        self.reserved_cost += worst_cost.unwrap_or(0);
        self.in_flight += 1;
        self.next_id += 1;
        Ok(Reservation { id: self.next_id, tokens: worst_tokens, cost: worst_cost })
    }

    /// Settles a reservation with the provider-reported usage. If the call
    /// failed with unknown usage (e.g. cancelled after the provider accepted
    /// it), pass `None` and the worst case is charged.
    pub fn settle(&mut self, r: Reservation, actual: Option<(u64, Option<Micros>)>) {
        self.reserved_tokens -= r.tokens;
        self.reserved_cost -= r.cost.unwrap_or(0);
        self.in_flight -= 1;
        let (tokens, cost) = actual.unwrap_or((r.tokens, r.cost));
        self.spent.tokens += tokens;
        self.spent.calls += 1;
        match cost {
            Some(c) => self.spent.cost += c,
            None => self.spent.cost_unknown = true,
        }
    }

    /// Releases a reservation for a call that was never sent.
    pub fn release(&mut self, r: Reservation) {
        self.reserved_tokens -= r.tokens;
        self.reserved_cost -= r.cost.unwrap_or(0);
        self.in_flight -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(tokens: u64, cost: Micros) -> Limits {
        Limits { max_tokens: Some(tokens), max_cost: Some(cost), max_calls: None, allow_unknown_cost: false }
    }

    #[test]
    fn concurrent_reservations_cannot_exceed_budget() {
        let mut g = Guard::new(limits(1000, 1_000), Totals::default());
        let a = g.reserve(400, Some(300)).unwrap();
        let b = g.reserve(400, Some(300)).unwrap();
        assert!(matches!(g.reserve(400, Some(300)), Err(Refusal::Tokens { remaining: 200, .. })));
        // Settling below the worst case frees room.
        g.settle(a, Some((100, Some(50))));
        let c = g.reserve(400, Some(300)).unwrap();
        g.settle(b, Some((400, Some(300))));
        g.settle(c, Some((400, Some(300))));
        assert_eq!(g.spent(), &Totals { tokens: 900, cost: 650, cost_unknown: false, calls: 3 });
        assert_eq!(g.in_flight(), 0);
    }

    #[test]
    fn cost_limit_and_unknown_prices() {
        let mut g = Guard::new(limits(1_000_000, 500), Totals::default());
        assert!(matches!(g.reserve(10, Some(600)), Err(Refusal::Cost { .. })));
        assert_eq!(g.reserve(10, None), Err(Refusal::UnknownCost));
        let mut g = Guard::new(Limits { allow_unknown_cost: true, ..limits(1_000_000, 500) }, Totals::default());
        let r = g.reserve(10, None).unwrap();
        g.settle(r, Some((10, None)));
        assert!(g.spent().cost_unknown, "unknown cost is tracked, not counted as zero");
    }

    #[test]
    fn unknown_actual_usage_charges_worst_case() {
        let mut g = Guard::new(limits(1000, 1000), Totals::default());
        let r = g.reserve(300, Some(200)).unwrap();
        g.settle(r, None);
        assert_eq!((g.spent().tokens, g.spent().cost), (300, 200));
        let r = g.reserve(100, Some(100)).unwrap();
        g.release(r);
        assert_eq!(g.spent().calls, 1);
    }

    #[test]
    fn prior_spend_calls_and_pause() {
        let prior = Totals { tokens: 900, cost: 0, cost_unknown: false, calls: 1 };
        let mut g = Guard::new(Limits { max_calls: Some(2), ..limits(1000, 1000) }, prior);
        assert!(g.reserve(200, Some(1)).is_err(), "daily spend counts");
        let r = g.reserve(50, Some(1)).unwrap();
        assert_eq!(g.reserve(10, Some(1)), Err(Refusal::Calls));
        g.release(r);
        g.pause();
        assert_eq!(g.reserve(10, Some(1)), Err(Refusal::Paused));
        g.resume();
        assert!(g.reserve(10, Some(1)).is_ok());
    }
}
