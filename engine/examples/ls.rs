//! Prints a folder as a scan recorded it, with flags. Reads only the store.
//! cargo run -p diskdiff-engine --example ls -- <store.db> <abs path> [scan id]
use diskdiff_engine::{Store, node, query};

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let (db, path) = (a.next().unwrap(), a.next().unwrap());
    let store = Store::open(db.as_ref())?;
    let scans = store.scans()?;
    let scan = match a.next() {
        Some(id) => scans.iter().find(|s| s.id.to_string() == id).unwrap().clone(),
        None => scans.iter().rev().find(|s| !s.temporary).unwrap().clone(),
    };
    println!("scan {} of {}", scan.id, scan.root);
    let rel = query::rel_in(&scan, path.as_ref()).unwrap_or_default();
    match store.resolve(&scan.root_hash, &rel)? {
        None => println!("{path}: not in this scan"),
        Some((_, n)) => {
            for d in &n.dirs {
                let mut f = Vec::new();
                if d.flags & node::FLAG_UNREADABLE != 0 { f.push("unreadable") }
                if d.flags & node::FLAG_CLOUD != 0 { f.push("cloud") }
                if d.flags & node::FLAG_PACKAGE != 0 { f.push("package") }
                println!("  dir  {:>12}  {:<40} {}", d.size, d.name, f.join(","));
            }
            for x in &n.files {
                println!("  file {:>12}  {}", x.size, x.name);
            }
        }
    }
    Ok(())
}
