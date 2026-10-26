//! Side-by-side placement for overlapping blocks, the way Outlook lays out a busy day.

use crate::TimeRange;

/// Where a block sits horizontally: column `column` of `columns` equal columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub column: usize,
    pub columns: usize,
}

impl Placement {
    /// Left edge and width as fractions of the lane.
    pub fn fractions(&self) -> (f32, f32) {
        let width = 1.0 / self.columns as f32;
        (self.column as f32 * width, width)
    }
}

/// Places each range in a column so overlapping ranges sit side by side. Ranges that
/// overlap, directly or through a chain of others, share a column count. The result is in
/// the same order as `ranges`.
pub fn place(ranges: &[TimeRange]) -> Vec<Placement> {
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    // Longer blocks first on ties, so they take the leftmost column.
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&ranges[a], &ranges[b]);
        ra.start.cmp(&rb.start).then(rb.end.cmp(&ra.end))
    });

    let mut placements = vec![Placement { column: 0, columns: 1 }; ranges.len()];
    let mut cluster: Vec<usize> = Vec::new();
    // The end of the last block in each column of the current cluster.
    let mut column_ends: Vec<jiff::Timestamp> = Vec::new();
    let mut cluster_end: Option<jiff::Timestamp> = None;

    let close_cluster =
        |cluster: &mut Vec<usize>, column_ends: &mut Vec<jiff::Timestamp>, out: &mut [Placement]| {
            for &i in cluster.iter() {
                out[i].columns = column_ends.len();
            }
            cluster.clear();
            column_ends.clear();
        };

    for i in order {
        let range = ranges[i];
        if cluster_end.is_some_and(|end| range.start >= end) {
            close_cluster(&mut cluster, &mut column_ends, &mut placements);
            cluster_end = None;
        }
        let column = match column_ends.iter().position(|&end| end <= range.start) {
            Some(free) => {
                column_ends[free] = range.end;
                free
            }
            None => {
                column_ends.push(range.end);
                column_ends.len() - 1
            }
        };
        placements[i].column = column;
        cluster.push(i);
        cluster_end = Some(cluster_end.map_or(range.end, |end| end.max(range.end)));
    }
    close_cluster(&mut cluster, &mut column_ends, &mut placements);
    placements
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::at;
    use jiff::{civil::date, tz::TimeZone};

    fn range(from: (i8, i8), to: (i8, i8)) -> TimeRange {
        let day = date(2026, 10, 26);
        let tz = TimeZone::UTC;
        TimeRange::new(at(day, from.0, from.1, &tz), at(day, to.0, to.1, &tz))
    }

    fn columns(placements: &[Placement]) -> Vec<(usize, usize)> {
        placements.iter().map(|p| (p.column, p.columns)).collect()
    }

    #[test]
    fn separate_blocks_get_the_full_width() {
        let placed = place(&[range((9, 0), (10, 0)), range((10, 0), (11, 0))]);
        assert_eq!(columns(&placed), [(0, 1), (0, 1)]);
    }

    #[test]
    fn overlapping_blocks_sit_side_by_side() {
        let placed = place(&[range((9, 0), (11, 0)), range((10, 0), (12, 0))]);
        assert_eq!(columns(&placed), [(0, 2), (1, 2)]);
    }

    #[test]
    fn a_chain_of_overlaps_shares_one_column_count() {
        // A overlaps B, B overlaps C, but A and C don't touch: C reuses A's column.
        let placed = place(&[
            range((9, 0), (10, 0)),
            range((9, 30), (11, 0)),
            range((10, 0), (10, 30)),
            range((13, 0), (14, 0)),
        ]);
        assert_eq!(columns(&placed), [(0, 2), (1, 2), (0, 2), (0, 1)]);
    }

    #[test]
    fn keeps_input_order() {
        let placed = place(&[range((10, 0), (12, 0)), range((9, 0), (11, 0))]);
        assert_eq!(columns(&placed), [(1, 2), (0, 2)]);
    }
}
