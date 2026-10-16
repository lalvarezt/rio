use rio_backend::crosswords::pos::{Column, Direction, Line, Pos};
use rio_backend::crosswords::search::Match;
use rio_backend::crosswords::search::{RegexIter, RegexSearch};
use rio_backend::crosswords::Crosswords;

/// Maximum number of linewraps followed outside of the viewport during search highlighting.
pub const MAX_SEARCH_LINES: usize = 100;

/// Iterate over all visible regex matches.
pub fn visible_regex_match_iter<'a, T: rio_backend::event::EventListener>(
    term: &'a Crosswords<T>,
    regex: &'a mut RegexSearch,
) -> impl Iterator<Item = Match> + 'a {
    let viewport_start = Line(-(term.grid.display_offset() as i32));
    let viewport_end = viewport_start + term.bottommost_line();
    let mut start = term.line_search_left(Pos::new(viewport_start, Column(0)));
    let mut end = term.line_search_right(Pos::new(viewport_end, Column(0)));
    start.row = start.row.max(viewport_start - MAX_SEARCH_LINES);
    end.row = end.row.min(viewport_end + MAX_SEARCH_LINES);

    RegexIter::new(start, end, Direction::Right, term, regex)
        .skip_while(move |rm| rm.end().row < viewport_start)
        .take_while(move |rm| rm.start().row <= viewport_end)
}
