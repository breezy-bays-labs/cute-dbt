//! End-to-end coverage of the cute-dbt#126 external fixture FILE reader.
//!
//! The unit + headless tests stub the reader (in-memory `StubReader`) or
//! construct `ExternalFixtures` directly, bypassing the run loop's
//! `gather_external_fixtures` stage. This test exercises the ONE production
//! seam none of those touch: a real dbt-fusion-shaped manifest
//! (`fixture: "tests/fixtures/X.csv"`, `rows: null`) → the cli stage
//! resolving `--project-root` → constructing `FsProjectFileReader` → reading
//! each resolved path off a real temp filesystem → the loaded cell grid in
//! the rendered report.
//!
//! The CSV row data exists ONLY on disk (the manifest carries `rows: null`),
//! so a per-side marker appearing in the output proves the whole chain ran.

use std::path::Path;
use std::process::Command;

/// Absolute path to the committed synthetic v12 manifest fixture.
fn committed_manifest() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/external-fixture-reader.json")
        .to_str()
        .expect("fixture path is valid UTF-8")
        .to_owned()
}

#[test]
fn external_fixture_files_are_read_through_project_root_and_inlined() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("external_fixture_reader_e2e");
    let _ = std::fs::remove_dir_all(&root);
    let fixtures = root.join("tests").join("fixtures");
    std::fs::create_dir_all(&fixtures).expect("mkdir temp project fixtures dir");

    // The row data lives ONLY here — the manifest's given/expect carry
    // `rows: null`. A unique marker per side proves the reader read each file.
    std::fs::write(
        fixtures.join("orders_seed.csv"),
        "id,note\n1,GIVEN_FROM_FILE\n",
    )
    .expect("write the external given csv");
    std::fs::write(
        fixtures.join("orders_expected.csv"),
        "id,note\n1,EXPECT_FROM_FILE\n",
    )
    .expect("write the external expect csv");

    // A `--pr-diff` patch touching the unit-test YAML puts the test (and its
    // target model) in scope so it renders. The YAML file itself need not
    // exist on disk — the authoring-YAML drawer soft-fails; the external
    // fixture grid is what we assert.
    let patch = root.join("pr.patch");
    std::fs::write(
        &patch,
        "--- a/models/_unit_tests.yml\n\
         +++ b/models/_unit_tests.yml\n\
         @@ -1 +1 @@\n\
         -  - name: test_orders_external_fixture\n\
         +  - name: test_orders_external_fixture  # touched\n",
    )
    .expect("write the pr-diff patch");

    let out = root.join("report.html");
    let status = Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args([
            "--manifest",
            &committed_manifest(),
            "--pr-diff",
            &format!("@{}", patch.to_str().expect("patch path is valid UTF-8")),
            "--project-root",
            root.to_str().expect("project root is valid UTF-8"),
            "--out",
            out.to_str().expect("out path is valid UTF-8"),
        ])
        .status()
        .expect("the cute-dbt binary spawns");
    assert!(status.success(), "cute-dbt exits 0");

    let html = std::fs::read_to_string(&out).expect("report.html was written");
    assert!(
        html.contains("GIVEN_FROM_FILE"),
        "the external GIVEN csv was read off disk and inlined into the report",
    );
    assert!(
        html.contains("EXPECT_FROM_FILE"),
        "the external EXPECT csv was read off disk and inlined into the report",
    );
    assert!(
        html.contains("tests/fixtures/orders_seed.csv"),
        "the resolved fixture path is surfaced as provenance",
    );
}
