//! Local packages: the container, the resolution rule, and the resolver.
//!
//! Step 89 asks for local package and dependency support, kept simple, with no
//! registry. Every test here is one of the three promises that makes "simple" a
//! design rather than an omission.

use lazalith_os::{
    Container, LzaError, LzaPackage, LzaResource, PackageVersion, ResolveError, Resolved, resolve,
};
use lazalith_toolchain::{
    Architecture, Manifest, ManifestError, ResolveError as DependencyError, Resolver,
    VersionRequirement,
};

// -- the container --------------------------------------------------------

fn image() -> Vec<u8> {
    lazalith_toolchain::assemble_and_link(
        ".arch lz64\n.entry _start\n.global _start\n.section .text\n_start:\n    HALT\n",
    )
    .expect("the program links")
    .to_bytes()
    .expect("the image serialises")
}

const MANIFEST: &[u8] = b"[application]\nname = \"hello\"\nversion = \"1.2.3\"\nentry = \"src/main.lz\"\narchitecture = \"any\"\n";

fn package() -> LzaPackage {
    LzaPackage::new(
        b"hello",
        PackageVersion::new(1, 2, 3),
        lazalith_os::LzxArchitecture::Lz64,
        MANIFEST,
        &image(),
        &[],
    )
    .expect("the package builds")
}

#[test]
fn a_package_round_trips_and_is_a_fixed_point() {
    let once = package().to_bytes().expect("the package serialises");
    let read = LzaPackage::from_bytes(&once).expect("the package reads back");
    assert_eq!(
        read,
        package(),
        "a package did not survive its own encoding"
    );
    assert_eq!(
        read.to_bytes().expect("it re-encodes"),
        once,
        "packing twice gave two different files"
    );
}

#[test]
fn a_package_reads_its_identity_out_of_the_manifest() {
    let read =
        LzaPackage::from_bytes(&package().to_bytes().expect("serialises")).expect("reads back");
    assert_eq!(read.identity.name, b"hello");
    assert_eq!(read.identity.version, PackageVersion::new(1, 2, 3));
    assert_eq!(
        read.identity.architecture,
        lazalith_os::LzxArchitecture::Lz64
    );
}

#[test]
fn a_package_carries_the_executable_whole_and_never_its_own_opinion_of_it() {
    let built = image();
    let read =
        LzaPackage::from_bytes(&package().to_bytes().expect("serialises")).expect("reads back");
    assert_eq!(
        read.image, built,
        "the package does not hold the executable verbatim"
    );
    assert_eq!(
        read.image().expect("the image loads"),
        lazalith_os::LzxImage::from_bytes(&built).expect("the image reads back"),
        "the package's own image reader disagrees with the image reader"
    );
}

/// The design's rule 3, and the property step 86's fuzzer earned: a reader that
/// tolerates a non-canonical offset produces files that read cleanly and write back
/// as different files.
#[test]
fn a_non_canonical_offset_is_refused_rather_than_tolerated() {
    let good = package().to_bytes().expect("serialises");
    // The header's `name` offset, at 24, must be where the writer would put it.
    let wrong = u32::from_le_bytes([good[24], good[25], good[26], good[27]]).wrapping_add(8);
    let mut corrupted = good.clone();
    corrupted[24..28].copy_from_slice(&wrong.to_le_bytes());
    assert!(
        matches!(
            LzaPackage::from_bytes(&corrupted),
            Err(LzaError::Offset {
                field: "the name",
                ..
            })
        ),
        "a package whose name is not where the writer would put it was accepted"
    );
    assert_eq!(
        LzaPackage::from_bytes(&good).expect("the good one still reads"),
        package()
    );
}

#[test]
fn a_count_larger_than_the_file_is_refused_before_anything_is_allocated() {
    let mut bytes = package().to_bytes().expect("serialises");
    bytes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        matches!(
            LzaPackage::from_bytes(&bytes),
            Err(LzaError::Count {
                what: "resources",
                ..
            })
        ),
        "a package claiming four billion resources was accepted"
    );
}

#[test]
fn a_package_whose_manifest_and_table_disagree_is_refused() {
    let declaring = b"[application]\nname = \"hello\"\nversion = \"1.0.0\"\nentry = \"m.lz\"\narchitecture = \"any\"\n\n[resources]\nlogo = \"a.rgb\"\nsound = \"b.wav\"\n";
    let table = vec![LzaResource::label(b"logo"), LzaResource::label(b"sound")];
    let built = LzaPackage::new(
        b"hello",
        PackageVersion::new(1, 0, 0),
        lazalith_os::LzxArchitecture::Lz64,
        declaring,
        &image(),
        &table,
    )
    .expect("the package builds");
    let bytes = built.to_bytes().expect("serialises");
    assert_eq!(
        LzaPackage::from_bytes(&bytes).expect("reads back"),
        built,
        "a package with matching manifest and table did not round trip"
    );
    // Now change one name in the table only, keeping every length identical, so the
    // only thing wrong is that the table and the manifest disagree.
    let mut corrupted = bytes;
    let position = corrupted
        .windows(4)
        .position(|window| window == b"logo")
        .expect("the name is in there");
    corrupted[position..position + 4].copy_from_slice(b"L0G0");
    assert!(
        matches!(
            LzaPackage::from_bytes(&corrupted),
            Err(LzaError::UnorderedResources)
        ),
        "a package whose table named something its manifest does not was accepted"
    );
}

#[test]
fn a_resource_the_manifest_does_not_declare_is_refused_at_build_time() {
    let manifest = b"[application]\nname = \"hello\"\nversion = \"1.0.0\"\nentry = \"m.lz\"\narchitecture = \"any\"\n";
    let error = LzaPackage::new(
        b"hello",
        PackageVersion::new(1, 0, 0),
        lazalith_os::LzxArchitecture::Lz64,
        manifest,
        &image(),
        &[LzaResource::label(b"undeclared")],
    )
    .expect_err("a table naming an undeclared resource was accepted");
    assert!(
        matches!(error, LzaError::UndeclaredResource { .. }),
        "{error}"
    );
}

#[test]
fn an_illegal_name_is_refused_with_a_reason() {
    for name in [
        &b""[..],
        b"-leading",
        b"trailing-",
        b"Upper",
        b"has space",
        b"has/slash",
    ] {
        let error = LzaPackage::new(
            name,
            PackageVersion::new(1, 0, 0),
            lazalith_os::LzxArchitecture::Lz64,
            MANIFEST,
            &image(),
            &[],
        )
        .expect_err("an illegal name was accepted");
        assert!(
            matches!(error, LzaError::BadName { .. } | LzaError::TooLong { .. }),
            "{name:?} was refused with {error}, which does not say what was wrong"
        );
    }
}

#[test]
fn a_version_is_ordinal_not_lexical() {
    // The bug a string comparison makes, asserted so it cannot come back.
    assert!(PackageVersion::new(0, 10, 0) > PackageVersion::new(0, 9, 0));
    assert!(PackageVersion::new(1, 0, 0) > PackageVersion::new(0, 99, 99));
    assert_eq!(
        PackageVersion::parse("0.10.0").expect("parses"),
        PackageVersion::new(0, 10, 0)
    );
    for bad in ["1", "1.2.3.4", "v1.2.3", "1.2.3-beta", "", "a.b.c"] {
        assert!(
            PackageVersion::parse(bad).is_err(),
            "{bad:?} was read as a version"
        );
    }
}

// -- the resolution rule --------------------------------------------------

#[test]
fn resolution_decides_by_content_and_never_by_name() {
    let built = image();
    assert_eq!(Container::of(&built), Container::Image);
    assert_eq!(
        Container::of(&package().to_bytes().expect("serialises")),
        Container::Package
    );
    // A path is not consulted, so the same bytes resolve the same way whatever
    // they were called. This is the design's rule and the only test that could
    // catch a resolver that grew a name check.
    assert!(matches!(
        resolve(&built).expect("an image resolves"),
        Resolved::Image(_)
    ));
    assert!(
        resolve(&built)
            .expect("an image resolves")
            .into_image()
            .is_ok()
    );
    let packed = package().to_bytes().expect("serialises");
    assert!(resolve(&packed).expect("a package resolves").is_package());
    assert!(
        resolve(&packed)
            .expect("a package resolves")
            .into_image()
            .is_ok()
    );
}

#[test]
fn bytes_that_are_neither_are_not_executable() {
    for bytes in [&b""[..], b"#!/bin/sh\n", &[0_u8; 64][..]] {
        assert!(
            matches!(resolve(bytes), Err(ResolveError::NotExecutable { .. })),
            "{bytes:?} resolved to something runnable"
        );
    }
    // A file whose magic is right and whose contents are not is that reader's
    // problem to report, and it does — with a specific error, not a fallback.
    let mut broken = image();
    broken[8] = 0xff;
    assert!(matches!(resolve(&broken), Err(ResolveError::Image(_))));
    let mut broken = package().to_bytes().expect("serialises");
    broken[0] = b'X';
    broken[1] = b'Y';
    assert!(resolve(&broken).is_err());
}

#[test]
fn a_package_and_a_bare_executable_are_interchangeable_to_a_caller() {
    // The property that makes a package a container rather than a second format.
    let from_package = resolve(&package().to_bytes().expect("serialises"))
        .expect("a package resolves")
        .into_image()
        .expect("its image loads");
    let from_image = resolve(&image())
        .expect("an image resolves")
        .into_image()
        .expect("its image loads");
    assert_eq!(from_package, from_image);
}

// -- the manifest ---------------------------------------------------------

fn manifest_text(extra: &str) -> String {
    format!(
        "[application]\nname = \"hello\"\nversion = \"1.2.3\"\nentry = \"src/main.lz\"\narchitecture = \"any\"\n{extra}"
    )
}

#[test]
fn a_manifest_reads_the_four_required_fields() {
    let manifest = Manifest::parse(manifest_text("").as_bytes()).expect("the manifest reads");
    assert_eq!(manifest.name, "hello");
    assert_eq!(manifest.version, PackageVersion::new(1, 2, 3));
    assert_eq!(manifest.entry, "src/main.lz");
    assert_eq!(manifest.architecture, Architecture::Any);
    assert!(manifest.resources.is_empty());
    assert!(manifest.dependencies.is_empty());
}

#[test]
fn a_manifest_without_a_required_field_says_which_one() {
    for (field, text) in [
        (
            "name",
            "[application]\nversion = \"1.0.0\"\nentry = \"m.lz\"\narchitecture = \"any\"\n",
        ),
        (
            "version",
            "[application]\nname = \"a\"\nentry = \"m.lz\"\narchitecture = \"any\"\n",
        ),
        (
            "entry",
            "[application]\nname = \"a\"\nversion = \"1.0.0\"\narchitecture = \"any\"\n",
        ),
        (
            "architecture",
            "[application]\nname = \"a\"\nversion = \"1.0.0\"\nentry = \"m.lz\"\n",
        ),
    ] {
        let error = Manifest::parse(text.as_bytes()).expect_err("a manifest with no field parsed");
        assert_eq!(
            error,
            ManifestError::Missing {
                section: "application",
                field,
            },
            "the wrong field was named"
        );
    }
}

#[test]
fn a_manifest_in_a_shape_this_build_does_not_read_is_refused() {
    // The reader claims to read the manifest format and not TOML, and this is what
    // that claim costs. A section it does not know is an error, not a best effort.
    let error = Manifest::parse(
        b"[application]\nname = \"a\"\nversion = \"1.0.0\"\nentry = \"m.lz\"\narchitecture = \"any\"\n\n[[bin]]\nname = \"x\"\n",
    )
    .expect_err("a table array was accepted");
    assert!(
        matches!(error, ManifestError::UnknownSection { .. }),
        "{error}"
    );
    // And a field before any section.
    let error =
        Manifest::parse(b"name = \"a\"\n").expect_err("a field before a section was accepted");
    assert!(matches!(error, ManifestError::Syntax { .. }), "{error}");
    // And a capability it does not know.
    let error = Manifest::parse(manifest_text("\n[permissions]\nnetwork = true\n").as_bytes())
        .expect_err("an unknown capability was accepted");
    assert!(matches!(error, ManifestError::Value { .. }), "{error}");
}

#[test]
fn a_manifest_reads_resources_permissions_and_dependencies_in_order() {
    let manifest = Manifest::parse(
        manifest_text(
            "\n[resources]\nlogo = \"a.rgb\"\nsound = \"b.wav\"\n\n[permissions]\nconsole = true\ninput = true\n\n[dependencies]\nstd = \"0.1\"\nutil = \"1.2.3\"\n",
        )
        .as_bytes(),
    )
    .expect("the manifest reads");
    assert_eq!(manifest.resources, vec!["logo", "sound"]);
    assert_eq!(manifest.permissions, vec!["console", "input"]);
    assert_eq!(
        manifest
            .dependencies
            .get("std")
            .copied()
            .expect("std is a dependency"),
        VersionRequirement::caret(0, 1)
    );
    // BTreeMap, so the order is a decision and not an accident of insertion.
    let names: Vec<&str> = manifest.dependencies.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["std", "util"]);
}

#[test]
fn a_manifest_with_a_comment_and_a_quoted_hash_both_read_correctly() {
    let manifest = Manifest::parse(
        manifest_text("\n# a comment\n[resources]\nlogo = \"a#b.rgb\" # trailing\n").as_bytes(),
    )
    .expect("the manifest reads");
    assert_eq!(manifest.resources, vec!["logo"]);
}

// -- the resolver ---------------------------------------------------------

fn dependency_manifest(name: &str, version: &str, needs: &str) -> String {
    format!(
        "[application]\nname = \"{name}\"\nversion = \"{version}\"\nentry = \"m.lz\"\narchitecture = \"any\"\n{needs}"
    )
}

fn resolver_with(packages: &[(&str, String)]) -> Resolver {
    let mut resolver = Resolver::new();
    resolver.add_directory(
        "/packages",
        &packages
            .iter()
            .map(|(name, text)| (String::from(*name), text.clone()))
            .collect::<Vec<_>>(),
    );
    resolver
}

#[test]
fn a_local_dependency_resolves_to_the_highest_version_that_fits() {
    let resolver = resolver_with(&[
        ("util", dependency_manifest("util", "0.1.0", "")),
        ("util", dependency_manifest("util", "0.1.9", "")),
        ("util", dependency_manifest("util", "0.2.0", "")),
        ("util", dependency_manifest("util", "1.0.0", "")),
    ]);
    let root = Manifest::parse(manifest_text("\n[dependencies]\nutil = \"0.1\"\n").as_bytes())
        .expect("the manifest reads");
    let resolved = resolver.resolve(&root).expect("it resolves");
    assert_eq!(resolved.dependencies.len(), 1);
    // `0.1` is `>=0.1.0, <0.2.0`, so 0.1.9 wins and 0.2.0 and 1.0.0 do not.
    assert_eq!(
        resolved.dependencies[0].version,
        PackageVersion::new(0, 1, 9)
    );
}

#[test]
fn an_exact_requirement_resolves_to_that_version() {
    let resolver = resolver_with(&[
        ("util", dependency_manifest("util", "0.1.0", "")),
        ("util", dependency_manifest("util", "0.1.9", "")),
        ("util", dependency_manifest("util", "0.2.0", "")),
    ]);
    let root = Manifest::parse(manifest_text("\n[dependencies]\nutil = \"0.1.0\"\n").as_bytes())
        .expect("the manifest reads");
    let resolved = resolver.resolve(&root).expect("it resolves");
    assert_eq!(
        resolved.dependencies[0].version,
        PackageVersion::new(0, 1, 0)
    );
}

#[test]
fn a_dependency_resolves_transitively() {
    let resolver = resolver_with(&[
        (
            "util",
            dependency_manifest("util", "1.0.0", "\n[dependencies]\nbase = \"2.0\"\n"),
        ),
        ("base", dependency_manifest("base", "2.0.1", "")),
    ]);
    let root = Manifest::parse(manifest_text("\n[dependencies]\nutil = \"1.0\"\n").as_bytes())
        .expect("the manifest reads");
    let resolved = resolver.resolve(&root).expect("it resolves");
    assert_eq!(resolved.dependencies[0].name, "util");
    assert_eq!(resolved.dependencies[0].dependencies.len(), 1);
    assert_eq!(resolved.dependencies[0].dependencies[0].name, "base");
    assert_eq!(
        resolved.dependencies[0].dependencies[0].version,
        PackageVersion::new(2, 0, 1)
    );
}

#[test]
fn a_dependency_shared_by_two_dependents_is_resolved_once() {
    let resolver = resolver_with(&[
        (
            "util",
            dependency_manifest("util", "1.0.0", "\n[dependencies]\nbase = \"2.0\"\n"),
        ),
        (
            "extra",
            dependency_manifest("extra", "1.0.0", "\n[dependencies]\nbase = \"2.0\"\n"),
        ),
        ("base", dependency_manifest("base", "2.0.1", "")),
    ]);
    let root = Manifest::parse(
        manifest_text("\n[dependencies]\nutil = \"1.0\"\nextra = \"1.0\"\n").as_bytes(),
    )
    .expect("the manifest reads");
    let resolved = resolver.resolve(&root).expect("it resolves");
    let nested: Vec<&str> = resolved
        .dependencies
        .iter()
        .flat_map(|entry| entry.dependencies.iter().map(|inner| inner.name.as_str()))
        .collect();
    assert_eq!(nested, vec!["base", "base"]);
}

#[test]
fn a_missing_dependency_says_where_it_looked() {
    let resolver = resolver_with(&[("other", dependency_manifest("other", "1.0.0", ""))]);
    let root = Manifest::parse(manifest_text("\n[dependencies]\nutil = \"1.0\"\n").as_bytes())
        .expect("the manifest reads");
    let error = resolver
        .resolve(&root)
        .expect_err("a missing dependency resolved");
    let DependencyError::Missing { name, searched } = &error else {
        panic!("{error}");
    };
    assert_eq!(name, "util");
    assert_eq!(searched, &["/packages"]);
    assert!(
        error.to_string().contains("/packages"),
        "the message does not say where it looked: {error}"
    );
}

#[test]
fn a_dependency_cycle_is_reported_as_a_path() {
    let resolver = resolver_with(&[
        (
            "a",
            dependency_manifest("a", "1.0.0", "\n[dependencies]\nb = \"1.0\"\n"),
        ),
        (
            "b",
            dependency_manifest("b", "1.0.0", "\n[dependencies]\na = \"1.0\"\n"),
        ),
    ]);
    let root = Manifest::parse(manifest_text("\n[dependencies]\na = \"1.0\"\n").as_bytes())
        .expect("the manifest reads");
    let error = resolver.resolve(&root).expect_err("a cycle resolved");
    let DependencyError::Cycle { path } = &error else {
        panic!("{error}");
    };
    // The path starts at the root and ends by naming something it already named, so
    // it closes a loop rather than merely being long.
    assert_eq!(path.first().map(String::as_str), Some("hello"));
    let last = path.last().expect("the path is not empty");
    let earlier = path[..path.len() - 1].iter().any(|name| name == last);
    assert!(earlier, "the path does not close a loop: {path:?}");
    assert!(error.to_string().contains("->"), "{error}");
}

#[test]
fn a_package_whose_directory_name_differs_from_its_manifest_is_not_a_candidate() {
    // A search directory may hold anything, so a neighbour that is not a package is
    // skipped rather than fatal. A package under the *wrong* name is different: it
    // would resolve the wrong thing.
    let resolver = resolver_with(&[
        ("wrong", dependency_manifest("util", "1.0.0", "")),
        ("right", dependency_manifest("right", "1.0.0", "")),
    ]);
    let root = Manifest::parse(manifest_text("\n[dependencies]\nutil = \"1.0\"\n").as_bytes())
        .expect("the manifest reads");
    let error = resolver
        .resolve(&root)
        .expect_err("a misnamed package resolved");
    assert!(matches!(error, DependencyError::Missing { .. }), "{error}");
    assert_eq!(
        resolver.available(),
        vec![(String::from("right"), PackageVersion::new(1, 0, 0))],
        "a misnamed package was offered as available"
    );
}

#[test]
fn a_directory_with_a_broken_manifest_does_not_stop_the_search() {
    let mut resolver = Resolver::new();
    resolver.add_directory(
        "/packages",
        &[
            (
                String::from("broken"),
                String::from("this is not a manifest"),
            ),
            (
                String::from("util"),
                dependency_manifest("util", "1.0.0", ""),
            ),
        ],
    );
    let root = Manifest::parse(manifest_text("\n[dependencies]\nutil = \"1.0\"\n").as_bytes())
        .expect("the manifest reads");
    assert!(resolver.resolve(&root).is_ok());
}

#[test]
fn there_is_no_registry_and_no_network_in_any_of_it() {
    // The step says do not build a registry. A resolver with a URL in its type, or a
    // lock file it writes, would be one. What it has instead is a list of
    // directories the caller supplied, and nothing else.
    let mut resolver = Resolver::new();
    assert!(resolver.searched().is_empty());
    resolver.add_directory("/a", &[]);
    resolver.add_directory("/b", &[]);
    assert_eq!(resolver.searched(), ["/a", "/b"]);
}
