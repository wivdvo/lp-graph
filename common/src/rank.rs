//! Tier/division/LP <-> one continuous "absolute LP" number.
//! Iron IV 0 LP = 0, each division = 100, each tier = 400, Master+ = 2800 + LP.

pub const TIERS: [&str; 10] = [
    "IRON", "BRONZE", "SILVER", "GOLD", "PLATINUM", "EMERALD", "DIAMOND", "MASTER", "GRANDMASTER", "CHALLENGER",
];
pub const MASTER_ABS: i64 = 2800;

fn div_index(div: &str) -> i64 {
    match div {
        "IV" => 0,
        "III" => 1,
        "II" => 2,
        "I" => 3,
        _ => 0,
    }
}

pub fn to_abs(tier: &str, div: &str, lp: i64) -> Option<i64> {
    let t = TIERS.iter().position(|&x| x == tier)? as i64;
    if t >= 7 {
        Some(MASTER_ABS + lp)
    } else {
        Some(t * 400 + div_index(div) * 100 + lp)
    }
}

/// Inverse of `to_abs` (Master+ collapses to "MASTER").
pub fn from_abs(abs: i64) -> (String, String, i64) {
    let abs = abs.max(0);
    if abs >= MASTER_ABS {
        return ("MASTER".into(), String::new(), abs - MASTER_ABS);
    }
    let t = (abs / 400) as usize;
    let d = (abs % 400) / 100;
    let div = ["IV", "III", "II", "I"][d as usize];
    (TIERS[t].into(), div.into(), abs % 100)
}

pub fn pretty_tier(tier: &str) -> String {
    let mut c = tier.chars();
    match c.next() {
        Some(f) => f.to_string() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// "Gold II 16 LP"
pub fn label(tier: &str, div: &str, lp: i64) -> String {
    if div.is_empty() {
        format!("{} {} LP", pretty_tier(tier), lp)
    } else {
        format!("{} {} {} LP", pretty_tier(tier), div, lp)
    }
}

/// Label for a division gridline at absolute LP `abs` (multiple of 100): "Gold II".
pub fn division_name(abs: i64) -> String {
    let (t, d, _) = from_abs(abs);
    if d.is_empty() {
        pretty_tier(&t)
    } else {
        format!("{} {}", pretty_tier(&t), d)
    }
}

/// Queue type string (ranked stats) <-> match-history queue id.
pub const QUEUES: [(&str, i64, &str); 2] = [("RANKED_SOLO_5x5", 420, "Solo/Duo"), ("RANKED_FLEX_SR", 440, "Flex")];

pub fn queue_id(queue: &str) -> i64 {
    QUEUES.iter().find(|q| q.0 == queue).map(|q| q.1).unwrap_or(0)
}

/// Current standing in one queue, from /lol-ranked/v1/current-ranked-stats.
#[derive(Clone, Debug, PartialEq)]
pub struct Standing {
    pub tier: String,
    pub div: String,
    pub lp: i64,
    pub wins: i64,
    pub losses: i64,
    pub abs: i64,
}

pub fn standing(stats: &serde_json::Value, queue: &str) -> Option<Standing> {
    let q = &stats["queueMap"][queue];
    let tier = q["tier"].as_str().unwrap_or("").to_string();
    let div = q["division"].as_str().unwrap_or("").to_string();
    let lp = q["leaguePoints"].as_i64().unwrap_or(0);
    let abs = to_abs(&tier, &div, lp)?; // None while unranked / in placements
    let div = if abs >= MASTER_ABS { String::new() } else { div };
    Some(Standing {
        tier,
        div,
        lp,
        wins: q["wins"].as_i64().unwrap_or(0),
        losses: q["losses"].as_i64().unwrap_or(0),
        abs,
    })
}
