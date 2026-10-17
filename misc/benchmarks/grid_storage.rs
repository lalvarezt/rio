//! CPU row-storage costs, excluding uploads, command recording, and GPU waits.
use super::support;
use sugarloaf::grid::CellBg;

pub fn measure(prefix: &str, mut write: impl FnMut(u32, &[CellBg])) {
    const COLUMNS: usize = 160;
    const ROWS: u32 = 48;
    let mut cells = vec![CellBg::TRANSPARENT; COLUMNS];
    for mode in ["transparent", "opaque", "tail_change"] {
        cells.fill(if mode == "opaque" {
            CellBg {
                rgba: [30, 40, 50, 255],
            }
        } else {
            CellBg::TRANSPARENT
        });
        for row in 0..ROWS {
            write(row, &cells);
        }
        support::measure(
            &format!("{prefix}/row_storage/{mode}"),
            COLUMNS * ROWS as usize,
            || {
                if mode == "tail_change" {
                    cells[COLUMNS - 1].rgba[3] ^= 255;
                }
                for row in 0..ROWS {
                    write(row, std::hint::black_box(&cells));
                }
                cells[COLUMNS - 1].rgba[3] as u64
            },
        );
    }
    cells.fill(CellBg::TRANSPARENT);
    for row in 0..ROWS {
        write(row, &cells);
    }
}
