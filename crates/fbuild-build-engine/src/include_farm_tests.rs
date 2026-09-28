use super::*;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Block directories `d0..dN` under a fresh temp root.
fn block_dirs(tmp: &tempfile::TempDir, n: usize) -> Vec<NormalizedPath> {
    (0..n)
        .map(|i| {
            let dir = NormalizedPath::new(tmp.path().join(format!("d{i}")));
            std::fs::create_dir_all(dir.as_path()).unwrap();
            dir
        })
        .collect()
}

fn kept_indices(plan: &Plan) -> Vec<usize> {
    plan.kept.clone()
}

#[test]
fn unique_headers_are_all_farmed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 3);
    write(block[0].as_path(), "a.h", "");
    write(block[1].as_path(), "sub/b.h", "");
    write(block[2].as_path(), "c.h", "");

    let plan = plan_farm(&[], &block).unwrap();

    assert!(kept_indices(&plan).is_empty());
    let rels: Vec<&str> = plan.links.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(rels, ["a.h", "c.h", "sub"]);
}

#[test]
fn duplicated_header_keeps_both_directories_in_order() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 4);
    write(block[0].as_path(), "only0.h", "");
    write(block[1].as_path(), "endian.h", "");
    write(block[2].as_path(), "only2.h", "");
    write(block[3].as_path(), "endian.h", "");

    let plan = plan_farm(&[], &block).unwrap();

    assert_eq!(kept_indices(&plan), [1, 3]);
    let rels: Vec<&str> = plan.links.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(rels, ["only0.h", "only2.h"]);
}

#[test]
fn single_owner_subtree_is_one_directory_link() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 2);
    write(block[0].as_path(), "freertos/FreeRTOS.h", "");
    write(block[0].as_path(), "freertos/task.h", "");
    write(block[1].as_path(), "hal/gpio.h", "");
    write(block[0].as_path(), "hal/uart.h", "");

    let plan = plan_farm(&[], &block).unwrap();

    let links: Vec<(&str, NormalizedPath)> = plan
        .links
        .iter()
        .map(|(rel, target)| (rel.as_str(), target.clone()))
        .collect();
    assert!(links.contains(&("freertos", block[0].join("freertos"))));
    assert!(links.contains(&("hal/gpio.h", block[1].join("hal/gpio.h"))));
    assert!(links.contains(&("hal/uart.h", block[0].join("hal/uart.h"))));
    assert_eq!(plan.real_dirs, ["hal"]);
}

#[test]
fn quoted_include_that_would_change_inside_a_merged_dir_keeps_its_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 3);
    // d0/hal/a.h includes "b.h": originally not beside it, so the chain finds
    // d2/b.h. In the merged farm `hal/`, d1's hal/b.h would sit beside it.
    write(block[0].as_path(), "hal/a.h", "#include \"b.h\"\n");
    write(block[1].as_path(), "hal/b.h", "");
    write(block[2].as_path(), "b.h", "");

    let plan = plan_farm(&[], &block).unwrap();

    assert_eq!(kept_indices(&plan), [0]);
}

#[test]
fn sibling_supplied_by_another_dir_in_a_merged_dir_is_a_conflict() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 2);
    write(block[0].as_path(), "hal/a.h", "#include \"a_impl.h\"\n");
    write(block[0].as_path(), "hal/a_impl.h", "");
    write(block[1].as_path(), "hal/b.h", "#include \"a.h\"\n");

    let plan = plan_farm(&[], &block).unwrap();

    // d1/hal/b.h's "a.h" is not beside it originally and the chain has no
    // top-level a.h, but the merged farm hal/ holds d0's a.h: a conflict.
    assert_eq!(kept_indices(&plan), [1]);
}

#[test]
fn directory_before_the_block_shadowing_a_sibling_include_is_a_conflict() {
    let tmp = tempfile::TempDir::new().unwrap();
    let before = vec![NormalizedPath::new(tmp.path().join("core"))];
    write(before[0].as_path(), "config.h", "");
    let block = block_dirs(&tmp, 2);
    // Originally "config.h" from d0 resolves to core/config.h (d0 lacks it);
    // in the farm root, d1's config.h would sit beside it.
    write(block[0].as_path(), "a.h", "#include \"config.h\"\n");
    write(block[1].as_path(), "config.h", "");

    let plan = plan_farm(&before, &block).unwrap();

    assert_eq!(kept_indices(&plan), [0]);
}

#[test]
fn file_in_one_dir_and_directory_in_another_keeps_both() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 3);
    write(block[0].as_path(), "port", "");
    write(block[1].as_path(), "port/x.h", "");
    write(block[2].as_path(), "fine.h", "");

    let plan = plan_farm(&[], &block).unwrap();

    assert_eq!(kept_indices(&plan), [0, 1]);
}

#[cfg(unix)]
#[test]
fn farm_resolves_every_header_to_the_original_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 3);
    write(block[0].as_path(), "freertos/task.h", "task");
    write(block[1].as_path(), "hal/gpio.h", "gpio");
    write(block[2].as_path(), "hal/uart.h", "uart");
    write(block[2].as_path(), "top.h", "top");

    let farm = ensure_farm(&tmp.path().join("farms"), &[], &block).unwrap();

    for (rel, body) in [
        ("freertos/task.h", "task"),
        ("hal/gpio.h", "gpio"),
        ("hal/uart.h", "uart"),
        ("top.h", "top"),
    ] {
        assert_eq!(std::fs::read_to_string(farm.dir.join(rel)).unwrap(), body);
    }
    assert!(farm.kept.is_empty());
    assert!(
        farm.dir
            .join("freertos")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
    assert!(
        !farm
            .dir
            .join("hal")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn farm_path_is_deterministic_and_rebuilt_in_place() {
    let tmp = tempfile::TempDir::new().unwrap();
    let block = block_dirs(&tmp, 2);
    write(block[0].as_path(), "a.h", "");
    write(block[1].as_path(), "a.h", "");
    write(block[1].as_path(), "b.h", "");
    let farms = tmp.path().join("farms");

    let first = ensure_farm(&farms, &[], &block).unwrap();
    assert_eq!(first.kept, block, "a duplicated a.h keeps both dirs");
    let again = ensure_farm(&farms, &[], &block).unwrap();
    assert_eq!(first, again);

    std::fs::remove_dir_all(first.dir.as_path()).unwrap();
    let rebuilt = ensure_farm(&farms, &[], &block).unwrap();
    assert_eq!(first, rebuilt, "depfiles naming the farm stay valid");

    let other = ensure_farm(&farms, &[], &block[..1]).unwrap();
    assert_ne!(first.dir, other.dir);
}

#[test]
fn replacement_puts_the_farm_first_then_kept_dirs() {
    let farm = IncludeFarm {
        dir: NormalizedPath::from("/farm"),
        kept: vec![NormalizedPath::from("/d1"), NormalizedPath::from("/d3")],
    };
    assert_eq!(
        farm.replacement(),
        [
            NormalizedPath::from("/farm"),
            NormalizedPath::from("/d1"),
            NormalizedPath::from("/d3")
        ]
    );
    assert_eq!(
        farm.macro_prefix_map("fw"),
        format!(
            "-fmacro-prefix-map={}=fw",
            NormalizedPath::from("/farm").display()
        )
    );
}
