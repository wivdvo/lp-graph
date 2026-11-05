//! Estimating LP for games played before the tracker existed. Match history has each
//! game's result but no LP, so we walk backwards from a known LP using average gains.

use crate::{split_at, Split};

pub struct EstGame {
    /// Game start (unix ms).
    pub t: i64,
    pub win: bool,
}

/// Walks backwards from `anchor_abs` (absolute LP just before `anchor_t`) through `games`
/// (oldest first) and returns (LP after the game, LP change) for each, oldest first.
///
/// LP resets at the start of every split, so the walk must not run straight through a
/// split start. When it crosses back into the previous split it restarts from
/// `prev_split_end` (your rank at the end of that split, from the client). Splits older
/// than that have no known rank, so the walk simply continues through them.
pub fn walk_back(
    games: &[EstGame],
    anchor_abs: i64,
    anchor_t: i64,
    splits: &[Split],
    prev_split_end: Option<i64>,
    avg: (f64, f64),
) -> Vec<(i64, i64)> {
    let mut out = vec![(0, 0); games.len()];
    let mut cur = anchor_abs;
    let mut split = split_at(splits, anchor_t).map(|s| s.start);
    let mut reanchored = false;
    for i in (0..games.len()).rev() {
        let g = &games[i];
        let gs = split_at(splits, g.t).map(|s| s.start);
        if split.is_some() && gs.is_some() && gs != split {
            if !reanchored {
                if let Some(p) = prev_split_end {
                    cur = p;
                }
                reanchored = true;
            }
            split = gs;
        }
        let d = if g.win { avg.0.round() as i64 } else { -(avg.1.round() as i64) };
        out[i] = (cur.max(0), d);
        cur -= d;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S2: Split = Split { start: 1_000, end: 1_999, number: 2 };
    const S3: Split = Split { start: 2_000, end: 2_999, number: 3 };

    #[test]
    fn walk_stays_continuous_within_a_split() {
        let games = [EstGame { t: 2_100, win: true }, EstGame { t: 2_200, win: false }];
        let r = walk_back(&games, 1_000, 2_500, &[S2, S3], Some(500), (20.0, 15.0));
        // newest game ends at the anchor; the one before ends 15 higher (it was followed by a loss)
        assert_eq!(r, vec![(1_015, 20), (1_000, -15)]);
    }

    #[test]
    fn walk_restarts_at_previous_split_end() {
        let games = [EstGame { t: 1_100, win: true }, EstGame { t: 1_200, win: true }, EstGame { t: 2_100, win: false }];
        let r = walk_back(&games, 1_000, 2_500, &[S2, S3], Some(500), (20.0, 15.0));
        // split 3 game ends at the anchor; split 2 games end at the previous split's final rank (500)
        assert_eq!(r, vec![(480, 20), (500, 20), (1_000, -15)]);
    }

    #[test]
    fn walk_without_previous_rank_continues() {
        let games = [EstGame { t: 1_100, win: true }, EstGame { t: 2_100, win: false }];
        let r = walk_back(&games, 1_000, 2_500, &[S2, S3], None, (20.0, 15.0));
        assert_eq!(r, vec![(1_015, 20), (1_000, -15)]);
    }
}
