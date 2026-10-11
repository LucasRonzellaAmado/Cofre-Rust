use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub keep_last: u32,
    pub keep_hourly: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    pub keep_yearly: u32,
}

impl RetentionPolicy {
    pub fn is_empty(&self) -> bool {
        self.keep_last == 0
            && self.keep_hourly == 0
            && self.keep_daily == 0
            && self.keep_weekly == 0
            && self.keep_monthly == 0
            && self.keep_yearly == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub time: i64,
    pub group: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub keep: Vec<String>,
    pub remove: Vec<String>,
}

fn local_time(ts: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(ts, 0).earliest()
}

fn bucket(kind: usize, ts: i64, id: &str) -> String {
    let Some(t) = local_time(ts) else {
        return format!("invalid-{id}");
    };
    match kind {
        0 => format!("last-{id}"),
        1 => format!("{}-{}-{}", t.year(), t.ordinal(), t.hour()),
        2 => format!("{}-{}", t.year(), t.ordinal()),
        3 => {
            let w = t.iso_week();
            format!("{}-W{}", w.year(), w.week())
        }
        4 => format!("{}-{}", t.year(), t.month()),
        _ => format!("{}", t.year()),
    }
}

pub fn apply_policy(candidates: &[Candidate], policy: &RetentionPolicy) -> Decision {
    if policy.is_empty() {
        return Decision {
            keep: candidates.iter().map(|c| c.id.clone()).collect(),
            remove: Vec::new(),
        };
    }
    let limits = [
        policy.keep_last,
        policy.keep_hourly,
        policy.keep_daily,
        policy.keep_weekly,
        policy.keep_monthly,
        policy.keep_yearly,
    ];
    let mut groups: BTreeMap<&str, Vec<&Candidate>> = BTreeMap::new();
    for c in candidates {
        groups.entry(c.group.as_str()).or_default().push(c);
    }
    let mut keep: HashSet<&str> = HashSet::new();
    for list in groups.values_mut() {
        list.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.id.cmp(&a.id)));
        for (kind, &limit) in limits.iter().enumerate() {
            if limit == 0 {
                continue;
            }
            let mut used = 0u32;
            let mut last: Option<String> = None;
            for c in list.iter() {
                if used >= limit {
                    break;
                }
                let b = bucket(kind, c.time, &c.id);
                if last.as_deref() != Some(b.as_str()) {
                    keep.insert(c.id.as_str());
                    used += 1;
                    last = Some(b);
                }
            }
        }
    }
    let mut decision = Decision::default();
    for c in candidates {
        if keep.contains(c.id.as_str()) {
            decision.keep.push(c.id.clone());
        } else {
            decision.remove.push(c.id.clone());
        }
    }
    decision
}
