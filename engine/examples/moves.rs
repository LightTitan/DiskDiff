//! Prints the moves between two scans. Reads only the store.
//! cargo run -p diskdiff-engine --example moves -- <store.db> [current id] [compare id]
use diskdiff_engine::{Store, moves};

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let store = Store::open(a.next().unwrap().as_ref())?;
    let scans: Vec<_> = store.scans()?.into_iter().filter(|s| !s.temporary).collect();
    let pick = |id: Option<String>, nth: usize| match id {
        Some(id) => scans.iter().find(|s| s.id.to_string() == id).unwrap().clone(),
        None => scans[scans.len() - 1 - nth].clone(),
    };
    let (cur, cmp) = (pick(a.next(), 0), pick(a.next(), 1));
    println!("scan {} ({}) against {} ({})", cur.id, cur.root, cmp.id, cmp.root);
    let m = moves::detect(&store, &cur, &cmp, &|| false)?;
    for mv in &m.list {
        println!("{:>10} {:>+10} {} {}\n                      -> {}", mv.size, mv.change(), if mv.is_dir { "dir " } else { "file" }, mv.from.display(), mv.to.display());
    }
    println!("{} moves, {} bytes", m.list.len(), m.total());
    Ok(())
}
