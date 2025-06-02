use fst::{IntoStreamer, Set};
use fst::automaton::Levenshtein;

fn main() -> Result<(), Box<dyn std::error::Error>> {
  // A convenient way to create sets in memory.
  let keys = vec!["abcde", "abcdf", "abcdg", "abcdh"];
  let set = Set::from_iter(keys)?;
  println!("Set: {set:?}");
  // Build our fuzzy query.
  let lev = Levenshtein::new("sa", 1)?;

  // Apply our fuzzy query to the set we built.
  let stream = set.search(lev).into_stream();

  let keys = stream.into_strs()?;
  Ok(())
}