//! What the connection this machine is on costs.
//!
//! Asking what the newest release is takes a few hundred bytes; the release is
//! megabytes. On a phone's hotspot or a capped plan the second is somebody's
//! money, so the updater asks either way and fetches only where bytes are not
//! counted - unless a person asked for the update there and then.
//!
//! The decision is [`Cost::network`], which knows nothing of Windows. [`now`]
//! is the thin call that fills a [`Cost`] in from
//! `Windows.Networking.Connectivity`.

use windows::Networking::Connectivity::{NetworkCostType, NetworkInformation};

use super::feed::Feed;

/// Whether the bytes of a download are somebody's to pay for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Network {
    /// Nothing counts the bytes.
    Open,
    /// A plan with a limit, or one billed by the byte.
    Metered,
}

/// How a connection is billed, as Windows names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Billing {
    /// Nothing is counted.
    Unrestricted,
    /// Counted against a limit.
    Fixed,
    /// Paid for by the byte.
    Variable,
    /// Windows does not know.
    Unknown,
}

/// What Windows says about the connection to the internet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cost {
    pub billing: Billing,
    /// Outside the home network, where a plan's own terms stop applying.
    pub roaming: bool,
    /// Past the plan's limit already.
    pub over_limit: bool,
}

impl Cost {
    /// Whether a download here is one to put off.
    ///
    /// A cost Windows does not know is taken as open: most connections say
    /// nothing about themselves, and a copy that waited on every one of them
    /// would never update. Roaming and a spent limit count whatever the plan
    /// is called - they are the two cases where an unrestricted plan still
    /// bills.
    pub fn network(self) -> Network {
        let counted = matches!(self.billing, Billing::Fixed | Billing::Variable);
        if counted || self.roaming || self.over_limit {
            Network::Metered
        } else {
            Network::Open
        }
    }
}

/// The connection as it is at this moment.
///
/// When Windows cannot say - no connection at all, or the question itself
/// failing - the answer is open. Being wrong that way costs one download on a
/// connection that did not say what it was; being wrong the other way is a
/// copy that stops updating and tells nobody why.
pub fn now() -> Network {
    if let Some(stood_in) = stood_in() {
        return stood_in;
    }
    read().map_or(Network::Open, Cost::network)
}

/// `NITID_UPDATE_NETWORK=metered`, for a gate that has no hotspot to stand
/// on. Heard only beside `NITID_UPDATE_FEED`, so nothing a person runs can be
/// told what its network is.
fn stood_in() -> Option<Network> {
    if !Feed::stood_in_for() {
        return None;
    }
    match std::env::var("NITID_UPDATE_NETWORK").ok()?.as_str() {
        "metered" => Some(Network::Metered),
        "open" => Some(Network::Open),
        _ => None,
    }
}

/// Ask Windows. An error is also what "no connection" looks like: the profile
/// comes back null.
fn read() -> windows::core::Result<Cost> {
    let cost = NetworkInformation::GetInternetConnectionProfile()?.GetConnectionCost()?;
    let billing = match cost.NetworkCostType()? {
        NetworkCostType::Unrestricted => Billing::Unrestricted,
        NetworkCostType::Fixed => Billing::Fixed,
        NetworkCostType::Variable => Billing::Variable,
        _ => Billing::Unknown,
    };
    Ok(Cost {
        billing,
        roaming: cost.Roaming()?,
        over_limit: cost.OverDataLimit()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(billing: Billing) -> Cost {
        Cost {
            billing,
            roaming: false,
            over_limit: false,
        }
    }

    #[test]
    fn a_counted_plan_is_metered_and_nothing_else_is() {
        assert_eq!(cost(Billing::Unrestricted).network(), Network::Open);
        assert_eq!(cost(Billing::Fixed).network(), Network::Metered);
        assert_eq!(cost(Billing::Variable).network(), Network::Metered);
    }

    /// Most connections say nothing about themselves. Were that metered, a
    /// copy on one of them would never update.
    #[test]
    fn a_cost_windows_does_not_know_is_open() {
        assert_eq!(cost(Billing::Unknown).network(), Network::Open);
    }

    /// The two ways an unrestricted plan still bills.
    #[test]
    fn roaming_and_a_spent_limit_count_whatever_the_plan() {
        for billing in [Billing::Unrestricted, Billing::Unknown] {
            assert_eq!(
                Cost {
                    roaming: true,
                    ..cost(billing)
                }
                .network(),
                Network::Metered
            );
            assert_eq!(
                Cost {
                    over_limit: true,
                    ..cost(billing)
                }
                .network(),
                Network::Metered
            );
        }
    }

    /// The updater's thread has set up no COM of its own, and the question
    /// has to be answerable from there: a refusal for that reason would read
    /// as "open" for ever and the whole rule would be decoration. No
    /// connection at all is a fair answer on a machine without one.
    #[test]
    fn windows_answers_a_thread_that_set_nothing_up() {
        let answer = std::thread::spawn(read).join().expect("the thread asking");
        if let Err(error) = answer {
            // CO_E_NOTINITIALIZED
            assert_ne!(error.code().0 as u32, 0x8004_01F0, "{error}");
        }
    }
}
