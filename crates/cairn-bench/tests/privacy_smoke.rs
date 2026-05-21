//! Privacy fixture runner smoke test — runs one minimal fixture against a real
//! temp vault via the subprocess driver and asserts the runner returns no leaks.

use cairn_bench::privacy::fixture::PrivacyFixture;
use cairn_bench::privacy::harness::run_fixture;

const SMOKE_FIXTURE: &str = r#"
scenario: smoke_fixture
description: minimal end-to-end privacy fixture
setup:
  records:
    - id: rec_smoke
      kind: reference
      body: "smoke body uniqueTokenTwelveThreeFour"
operations:
  - verb: forget
    target: rec_smoke
    mode: record
assertions:
  search:
    - mode: keyword
      query: "uniqueTokenTwelveThreeFour"
      must_not_contain_id: rec_smoke
"#;

#[test]
fn smoke_fixture_passes_against_real_vault() {
    let f: PrivacyFixture = yaml_serde::from_str(SMOKE_FIXTURE).unwrap();
    let failures = run_fixture(&f).expect("run fixture");
    assert!(failures.is_empty(), "expected pass; got: {failures:?}");
}
