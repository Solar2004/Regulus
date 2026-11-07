//! Policies at the door: cooldown, requests per window, concurrency and budget. The same
//! kind of checks `guards` makes on a wire (`budget`), made here per key, per workflow or
//! per key×workflow before a request reaches its workflow. Written here, not shared: the
//! guards keep theirs inside their module and modules do not share code.
//!
//! Every policy that applies to a request is checked; one that says no refuses it, with
//! the longest wait any of them asks for. A request let in counts in all of them at once.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// The limits of one policy. 0 is no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Milliseconds between two requests.
    pub cooldown_ms: u64,
    /// Requests per `window_ms`.
    pub requests: u64,
    pub window_ms: u64,
    /// Requests running at once.
    pub concurrency: u64,
    /// Requests ever let in.
    pub budget_requests: u64,
    /// Milliseconds of workflow time ever spent.
    pub budget_ms: u64,
}

impl Policy {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Sets one limit by name, as the CLI writes it (`requests=5`).
    pub fn set(&mut self, name: &str, value: u64) -> Result<(), String> {
        let slot = match name {
            "cooldown_ms" => &mut self.cooldown_ms,
            "requests" => &mut self.requests,
            "window_ms" => &mut self.window_ms,
            "concurrency" => &mut self.concurrency,
            "budget_requests" => &mut self.budget_requests,
            "budget_ms" => &mut self.budget_ms,
            other => {
                return Err(format!(
                    "unknown policy `{other}` (cooldown_ms, requests, window_ms, concurrency, \
                     budget_requests, budget_ms)"
                ))
            }
        };
        *slot = value;
        Ok(())
    }
}

/// What a budget has spent. Kept in the store: a restart does not refill it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Spent {
    pub requests: u64,
    pub ms: u64,
}

#[derive(Default)]
struct Counter {
    last_ms: Option<u64>,
    window: VecDeque<u64>,
    running: u64,
}

/// Why a request is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub reason: String,
    /// Seconds to wait; none when waiting does not help (a spent budget).
    pub retry_after_s: Option<u64>,
}

/// The counters of every scope, in memory; budgets come in through `spent`.
#[derive(Default)]
pub struct Counters {
    counters: HashMap<String, Counter>,
}

fn seconds(ms: u64) -> u64 {
    ms.div_ceil(1000).max(1)
}

impl Counters {
    /// Checks every `(scope, policy, spent)` at `now`; if all agree, counts the request in
    /// each and returns `Ok`.
    pub fn admit(&mut self, rules: &[(String, Policy, Spent)], now: u64) -> Result<(), Refused> {
        let mut refused: Option<Refused> = None;
        let mut refuse = |reason: String, wait: Option<u64>| {
            let longer = match (&refused, wait) {
                (None, _) => true,
                (Some(old), Some(new)) => old.retry_after_s.is_some_and(|o| new > o),
                (Some(_), None) => true,
            };
            if longer {
                refused = Some(Refused {
                    reason,
                    retry_after_s: wait,
                });
            }
        };
        for (scope, policy, spent) in rules {
            let counter = self.counters.entry(scope.clone()).or_default();
            if policy.window_ms > 0 {
                while counter
                    .window
                    .front()
                    .is_some_and(|at| at + policy.window_ms <= now)
                {
                    counter.window.pop_front();
                }
            }
            // Requests still running are reserved: they spend when they end.
            if policy.budget_requests > 0
                && spent.requests + counter.running >= policy.budget_requests
            {
                refuse(
                    format!(
                        "{scope}: budget of {} requests spent",
                        policy.budget_requests
                    ),
                    None,
                );
            }
            if policy.budget_ms > 0 && spent.ms >= policy.budget_ms {
                refuse(
                    format!("{scope}: budget of {} ms spent", policy.budget_ms),
                    None,
                );
            }
            if let (true, Some(last)) = (policy.cooldown_ms > 0, counter.last_ms) {
                let ready = last + policy.cooldown_ms;
                if now < ready {
                    refuse(
                        format!("{scope}: cooldown of {} ms", policy.cooldown_ms),
                        Some(seconds(ready - now)),
                    );
                }
            }
            if policy.requests > 0
                && policy.window_ms > 0
                && counter.window.len() as u64 >= policy.requests
            {
                let oldest = counter.window.front().copied().unwrap_or(now);
                let wait = (oldest + policy.window_ms).saturating_sub(now);
                refuse(
                    format!(
                        "{scope}: {} requests per {} ms",
                        policy.requests, policy.window_ms
                    ),
                    Some(seconds(wait)),
                );
            }
            if policy.concurrency > 0 && counter.running >= policy.concurrency {
                refuse(format!("{scope}: {} at once", policy.concurrency), Some(1));
            }
        }
        if let Some(refused) = refused {
            return Err(refused);
        }
        for (scope, policy, _) in rules {
            let counter = self.counters.entry(scope.clone()).or_default();
            counter.last_ms = Some(now);
            if policy.window_ms > 0 {
                counter.window.push_back(now);
            }
            counter.running += 1;
        }
        Ok(())
    }

    /// A request let in at `at` never started (a bad input, no room for a child): it no
    /// longer counts in the windows nor runs. A cooldown still started.
    pub fn refund(&mut self, scopes: &[String], at: u64) {
        for scope in scopes {
            if let Some(counter) = self.counters.get_mut(scope) {
                if let Some(i) = counter.window.iter().rposition(|t| *t == at) {
                    counter.window.remove(i);
                }
            }
        }
        self.release(scopes);
    }

    /// A request let in through `scopes` is over.
    pub fn release(&mut self, scopes: &[String]) {
        for scope in scopes {
            if let Some(counter) = self.counters.get_mut(scope) {
                counter.running = counter.running.saturating_sub(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(policy: Policy) -> Vec<(String, Policy, Spent)> {
        vec![("s".into(), policy, Spent::default())]
    }

    #[test]
    fn a_window_refuses_the_extra_request_and_says_when() {
        let rules = rule(Policy {
            requests: 2,
            window_ms: 10_000,
            ..Policy::default()
        });
        let mut counters = Counters::default();
        counters.admit(&rules, 1_000).unwrap();
        counters.admit(&rules, 2_000).unwrap();
        let refused = counters.admit(&rules, 3_000).unwrap_err();
        assert_eq!(refused.retry_after_s, Some(8));
        counters.admit(&rules, 11_000).unwrap();
    }

    #[test]
    fn cooldown_concurrency_and_budget() {
        let mut counters = Counters::default();
        let cool = rule(Policy {
            cooldown_ms: 1_500,
            ..Policy::default()
        });
        counters.admit(&cool, 0).unwrap();
        assert_eq!(
            counters.admit(&cool, 500).unwrap_err().retry_after_s,
            Some(1)
        );
        counters.admit(&cool, 1_500).unwrap();

        let one = vec![(
            "c".to_string(),
            Policy {
                concurrency: 1,
                ..Policy::default()
            },
            Spent::default(),
        )];
        counters.admit(&one, 0).unwrap();
        assert!(counters.admit(&one, 0).is_err());
        counters.release(&["c".into()]);
        counters.admit(&one, 0).unwrap();

        let spent = vec![(
            "b".to_string(),
            Policy {
                budget_requests: 3,
                ..Policy::default()
            },
            Spent { requests: 3, ms: 0 },
        )];
        assert_eq!(counters.admit(&spent, 0).unwrap_err().retry_after_s, None);
    }

    #[test]
    fn one_refusal_counts_in_no_scope() {
        let mut counters = Counters::default();
        let rules = vec![
            (
                "a".to_string(),
                Policy {
                    requests: 5,
                    window_ms: 1_000,
                    ..Policy::default()
                },
                Spent::default(),
            ),
            (
                "b".to_string(),
                Policy {
                    concurrency: 1,
                    ..Policy::default()
                },
                Spent::default(),
            ),
        ];
        counters.admit(&rules, 0).unwrap();
        assert!(counters.admit(&rules, 0).is_err());
        counters.release(&["a".into(), "b".into()]);
        // Four more fit in `a`: the refused one was not counted.
        for _ in 0..4 {
            counters.admit(&rules, 0).unwrap();
            counters.release(&["a".into(), "b".into()]);
        }
        assert!(counters.admit(&rules, 0).is_err());
    }
}
