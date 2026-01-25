//! W3C XSD Test Suite Conformance Driver
//!
//! This driver runs the W3C XSD 1.0/1.1 test suite to measure
//! conformance of the xsd-schema parser.
//!
//! # Test Suite Structure
//!
//! The W3C test suite is organized as:
//! ```text
//! xsdtests/
//! ├── suite.xml           # Test suite manifest
//! ├── nist/               # NIST tests
//! ├── sun/                # Sun Microsystems tests
//! ├── ms/                 # Microsoft tests
//! └── ibm/                # IBM tests
//! ```
//!
//! # Usage
//!
//! ```bash
//! cargo test --test conformance -- --test-suite /path/to/xsdtests
//! ```

#[path = "report.rs"]
mod report;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use quick_xml::events::Event;
use quick_xml::Reader;

/// Test result outcome
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    /// Test passed as expected
    Pass,
    /// Test failed when it should have passed
    Fail,
    /// Test was skipped (not applicable or not implemented)
    Skip,
    /// Test had an error (unexpected exception)
    Error,
}

/// A single test case result
#[derive(Debug, Clone)]
pub struct TestResult {
    /// Test case name/identifier
    pub name: String,
    /// Test group (contributor)
    pub group: String,
    /// Expected outcome from test suite
    pub expected: ExpectedOutcome,
    /// Actual outcome from running the test
    pub actual: TestOutcome,
    /// Duration of the test
    pub duration: Duration,
    /// Error message if any
    pub error_message: Option<String>,
}

/// Expected outcome from test definition
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedOutcome {
    /// Schema should be valid
    Valid,
    /// Schema should be invalid
    Invalid,
    /// Instance should validate against schema
    InstanceValid,
    /// Instance should NOT validate against schema
    InstanceInvalid,
}

/// Test suite statistics
#[derive(Debug, Clone, Default)]
pub struct TestStats {
    /// Number of tests that passed
    pub passed: usize,
    /// Number of tests that failed
    pub failed: usize,
    /// Number of tests that were skipped
    pub skipped: usize,
    /// Number of tests that had errors
    pub errors: usize,
    /// Total duration
    pub total_duration: Duration,
}

impl TestStats {
    /// Total number of tests run
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped + self.errors
    }

    /// Pass rate (0.0 - 1.0)
    pub fn pass_rate(&self) -> f64 {
        let total = self.passed + self.failed;
        if total == 0 {
            0.0
        } else {
            self.passed as f64 / total as f64
        }
    }

    /// Add a result to the stats
    pub fn add_result(&mut self, result: &TestResult) {
        match result.actual {
            TestOutcome::Pass => self.passed += 1,
            TestOutcome::Fail => self.failed += 1,
            TestOutcome::Skip => self.skipped += 1,
            TestOutcome::Error => self.errors += 1,
        }
        self.total_duration += result.duration;
    }
}

/// Test case from the suite manifest
#[derive(Debug, Clone)]
pub struct TestCase {
    /// Test name/id
    pub name: String,
    /// Schema file(s) to parse
    pub schema_files: Vec<PathBuf>,
    /// Instance document (if any)
    pub instance_file: Option<PathBuf>,
    /// Expected outcome
    pub expected: ExpectedOutcome,
    /// Test contributor/group
    pub group: String,
    /// Test version (1.0 or 1.1)
    pub version: String,
    /// Description
    pub description: Option<String>,
}

/// Test suite manifest parser
pub struct TestSuiteParser {
    base_path: PathBuf,
}

impl TestSuiteParser {
    /// Create a new parser for the given test suite directory
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    /// Parse the test suite manifest (suite.xml)
    pub fn parse_manifest(&self) -> Result<Vec<TestCase>, String> {
        let manifest_path = self.base_path.join("suite.xml");
        if !manifest_path.exists() {
            // Try alternative manifest names
            let alt_path = self.base_path.join("testSuite.xml");
            if alt_path.exists() {
                return self.parse_xml_manifest(&alt_path);
            }
            return Err(format!(
                "Test suite manifest not found at {:?}",
                manifest_path
            ));
        }
        self.parse_xml_manifest(&manifest_path)
    }

    /// Parse an XML manifest file
    fn parse_xml_manifest(&self, path: &Path) -> Result<Vec<TestCase>, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read manifest: {}", e))?;

        let mut reader = Reader::from_str(&content);
        reader.trim_text(true);

        let mut tests = Vec::new();
        let mut buf = Vec::new();

        // Current parsing state
        let mut current_group = String::new();
        let mut current_test: Option<TestCase> = None;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let name_ref = e.local_name();
                    let local_name = String::from_utf8_lossy(name_ref.as_ref()).to_string();

                    match local_name.as_str() {
                        "testGroup" => {
                            // Extract group name from attributes
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    current_group = String::from_utf8_lossy(&attr.value).to_string();
                                }
                            }
                        }
                        "schemaTest" => {
                            let mut test = TestCase {
                                name: String::new(),
                                schema_files: Vec::new(),
                                instance_file: None,
                                expected: ExpectedOutcome::Valid,
                                group: current_group.clone(),
                                version: "1.0".to_string(),
                                description: None,
                            };

                            // Extract attributes
                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"name" => {
                                        test.name = String::from_utf8_lossy(&attr.value).to_string();
                                    }
                                    b"version" => {
                                        test.version = String::from_utf8_lossy(&attr.value).to_string();
                                    }
                                    _ => {}
                                }
                            }

                            current_test = Some(test);
                        }
                        "schemaDocument" => {
                            if let Some(ref mut test) = current_test {
                                for attr in e.attributes().flatten() {
                                    if attr.key.as_ref() == b"xlink:href" || attr.key.as_ref() == b"href" {
                                        let href = String::from_utf8_lossy(&attr.value);
                                        let schema_path = self.base_path.join(href.as_ref());
                                        test.schema_files.push(schema_path);
                                    }
                                }
                            }
                        }
                        "expected" => {
                            if let Some(ref mut test) = current_test {
                                for attr in e.attributes().flatten() {
                                    if attr.key.as_ref() == b"validity" {
                                        let validity = String::from_utf8_lossy(&attr.value);
                                        test.expected = match validity.as_ref() {
                                            "valid" => ExpectedOutcome::Valid,
                                            "invalid" => ExpectedOutcome::Invalid,
                                            "notKnown" => ExpectedOutcome::Valid, // Treat as valid
                                            _ => ExpectedOutcome::Valid,
                                        };
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name_ref = e.local_name();
                    let local_name = String::from_utf8_lossy(name_ref.as_ref()).to_string();

                    match local_name.as_str() {
                        "testGroup" => {
                            current_group = String::new();
                        }
                        "schemaTest" => {
                            if let Some(test) = current_test.take() {
                                if !test.schema_files.is_empty() {
                                    tests.push(test);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(format!("XML parse error: {}", e));
                }
                _ => {}
            }
            buf.clear();
        }

        Ok(tests)
    }

    /// Scan directory for schema files if no manifest is found
    pub fn scan_for_tests(&self) -> Result<Vec<TestCase>, String> {
        let mut tests = Vec::new();
        self.scan_directory(&self.base_path, &mut tests)?;
        Ok(tests)
    }

    #[allow(clippy::only_used_in_recursion)]
    fn scan_directory(&self, dir: &Path, tests: &mut Vec<TestCase>) -> Result<(), String> {
        let entries = fs::read_dir(dir)
            .map_err(|e| format!("Failed to read directory {:?}: {}", dir, e))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.scan_directory(&path, tests)?;
            } else if path.extension().is_some_and(|e| e == "xsd") {
                // Create a test case for each .xsd file
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();

                let group = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "default".to_string());

                tests.push(TestCase {
                    name,
                    schema_files: vec![path],
                    instance_file: None,
                    expected: ExpectedOutcome::Valid, // Default expectation
                    group,
                    version: "1.0".to_string(),
                    description: None,
                });
            }
        }

        Ok(())
    }
}

/// Conformance test runner
pub struct TestRunner {
    /// Test cases to run
    tests: Vec<TestCase>,
    /// Filter by group name
    group_filter: Option<String>,
    /// Filter by test version
    version_filter: Option<String>,
    /// Maximum tests to run (0 = unlimited)
    max_tests: usize,
    /// Verbose output
    verbose: bool,
}

impl TestRunner {
    /// Create a new test runner
    pub fn new(tests: Vec<TestCase>) -> Self {
        Self {
            tests,
            group_filter: None,
            version_filter: None,
            max_tests: 0,
            verbose: false,
        }
    }

    /// Filter tests by group name
    pub fn with_group_filter(mut self, group: Option<String>) -> Self {
        self.group_filter = group;
        self
    }

    /// Filter tests by version
    pub fn with_version_filter(mut self, version: Option<String>) -> Self {
        self.version_filter = version;
        self
    }

    /// Limit the number of tests to run
    pub fn with_max_tests(mut self, max: usize) -> Self {
        self.max_tests = max;
        self
    }

    /// Enable verbose output
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Run all tests
    pub fn run(&self) -> (Vec<TestResult>, HashMap<String, TestStats>) {
        let mut results = Vec::new();
        let mut stats_by_group: HashMap<String, TestStats> = HashMap::new();

        let tests: Vec<_> = self
            .tests
            .iter()
            .filter(|t| {
                if let Some(ref group) = self.group_filter {
                    if !t.group.contains(group) {
                        return false;
                    }
                }
                if let Some(ref version) = self.version_filter {
                    if &t.version != version {
                        return false;
                    }
                }
                true
            })
            .take(if self.max_tests > 0 {
                self.max_tests
            } else {
                usize::MAX
            })
            .collect();

        println!("Running {} tests...", tests.len());

        for test in tests {
            let result = self.run_test(test);

            // Update group stats
            let stats = stats_by_group
                .entry(result.group.clone())
                .or_default();
            stats.add_result(&result);

            if self.verbose {
                let status = match result.actual {
                    TestOutcome::Pass => "PASS",
                    TestOutcome::Fail => "FAIL",
                    TestOutcome::Skip => "SKIP",
                    TestOutcome::Error => "ERROR",
                };
                println!(
                    "  {} - {} ({:?})",
                    status, result.name, result.duration
                );
                if let Some(ref msg) = result.error_message {
                    println!("    Error: {}", msg);
                }
            }

            results.push(result);
        }

        (results, stats_by_group)
    }

    /// Run a single test case
    fn run_test(&self, test: &TestCase) -> TestResult {
        let start = Instant::now();

        // Check if all schema files exist
        for schema_file in &test.schema_files {
            if !schema_file.exists() {
                return TestResult {
                    name: test.name.clone(),
                    group: test.group.clone(),
                    expected: test.expected,
                    actual: TestOutcome::Skip,
                    duration: start.elapsed(),
                    error_message: Some(format!("Schema file not found: {:?}", schema_file)),
                };
            }
        }

        // Try to parse the schema(s)
        let mut schema_set = xsd_schema::SchemaSet::new();
        let mut parse_error: Option<String> = None;

        // Phase 1: Parse all schemas
        for schema_file in &test.schema_files {
            match fs::read(schema_file) {
                Ok(content) => {
                    let uri = schema_file.to_string_lossy();
                    match xsd_schema::parse_schema_only(&content, &uri, &mut schema_set) {
                        Ok(_) => {}
                        Err(e) => {
                            parse_error = Some(e.to_string());
                            break;
                        }
                    }
                }
                Err(e) => {
                    parse_error = Some(format!("Failed to read file: {}", e));
                    break;
                }
            }
        }

        // Phase 2: Process all loaded schemas (inline assembly + reference resolution)
        if parse_error.is_none() {
            if let Err(e) = xsd_schema::process_loaded_schemas(&mut schema_set) {
                parse_error = Some(e.to_string());
            }
        }

        let duration = start.elapsed();

        // Determine outcome
        let (actual, error_message) = match (test.expected, &parse_error) {
            (ExpectedOutcome::Valid, None) => (TestOutcome::Pass, None),
            (ExpectedOutcome::Valid, Some(e)) => (TestOutcome::Fail, Some(e.clone())),
            (ExpectedOutcome::Invalid, None) => {
                (TestOutcome::Fail, Some("Schema was valid but expected invalid".to_string()))
            }
            (ExpectedOutcome::Invalid, Some(_)) => (TestOutcome::Pass, None),
            (ExpectedOutcome::InstanceValid, _) => {
                (TestOutcome::Skip, Some("Instance validation not implemented".to_string()))
            }
            (ExpectedOutcome::InstanceInvalid, _) => {
                (TestOutcome::Skip, Some("Instance validation not implemented".to_string()))
            }
        };

        TestResult {
            name: test.name.clone(),
            group: test.group.clone(),
            expected: test.expected,
            actual,
            duration,
            error_message,
        }
    }
}

/// Print a summary of test results
pub fn print_summary(stats_by_group: &HashMap<String, TestStats>) {
    println!("\n=== Test Summary ===\n");

    let mut total = TestStats::default();

    for (group, stats) in stats_by_group {
        println!(
            "{}: {} passed, {} failed, {} skipped, {} errors ({:.1}% pass rate)",
            group,
            stats.passed,
            stats.failed,
            stats.skipped,
            stats.errors,
            stats.pass_rate() * 100.0
        );
        total.passed += stats.passed;
        total.failed += stats.failed;
        total.skipped += stats.skipped;
        total.errors += stats.errors;
        total.total_duration += stats.total_duration;
    }

    println!(
        "\nTotal: {} tests, {} passed, {} failed, {} skipped, {} errors",
        total.total(),
        total.passed,
        total.failed,
        total.skipped,
        total.errors
    );
    println!(
        "Pass rate: {:.1}%",
        total.pass_rate() * 100.0
    );
    println!("Duration: {:?}", total.total_duration);
}

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse command line arguments
    let mut test_suite_path: Option<PathBuf> = None;
    let mut group_filter: Option<String> = None;
    let mut version_filter: Option<String> = None;
    let mut max_tests: usize = 0;
    let mut verbose = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--test-suite" | "-s" => {
                if i + 1 < args.len() {
                    test_suite_path = Some(PathBuf::from(&args[i + 1]));
                    i += 1;
                }
            }
            "--group" | "-g" => {
                if i + 1 < args.len() {
                    group_filter = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--version" | "-V" => {
                if i + 1 < args.len() {
                    version_filter = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--max" | "-m" => {
                if i + 1 < args.len() {
                    max_tests = args[i + 1].parse().unwrap_or(0);
                    i += 1;
                }
            }
            "--verbose" | "-v" => {
                verbose = true;
            }
            "--help" | "-h" => {
                println!("XSD Conformance Test Driver");
                println!();
                println!("Usage: conformance [OPTIONS]");
                println!();
                println!("Options:");
                println!("  -s, --test-suite PATH   Path to W3C test suite directory");
                println!("  -g, --group NAME        Filter by test group name");
                println!("  -V, --version VER       Filter by XSD version (1.0 or 1.1)");
                println!("  -m, --max NUM           Maximum number of tests to run");
                println!("  -v, --verbose           Enable verbose output");
                println!("  -h, --help              Show this help message");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    let test_suite_path = match test_suite_path {
        Some(p) => p,
        None => {
            println!("XSD Conformance Test Driver");
            println!();
            println!("No test suite path specified. Use --test-suite /path/to/xsdtests");
            println!();
            println!("To run conformance tests, you need the W3C XSD test suite.");
            println!("Download from: https://www.w3.org/XML/2004/xml-schema-test-suite/");
            return;
        }
    };

    if !test_suite_path.exists() {
        eprintln!("Error: Test suite path does not exist: {:?}", test_suite_path);
        std::process::exit(1);
    }

    // Parse test suite
    println!("Loading test suite from {:?}...", test_suite_path);
    let parser = TestSuiteParser::new(test_suite_path);

    let tests = match parser.parse_manifest() {
        Ok(t) => {
            println!("Loaded {} tests from manifest", t.len());
            t
        }
        Err(_) => {
            println!("No manifest found, scanning for .xsd files...");
            match parser.scan_for_tests() {
                Ok(t) => {
                    println!("Found {} test files", t.len());
                    t
                }
                Err(e) => {
                    eprintln!("Error scanning for tests: {}", e);
                    std::process::exit(1);
                }
            }
        }
    };

    if tests.is_empty() {
        println!("No tests found!");
        return;
    }

    // Run tests
    let runner = TestRunner::new(tests)
        .with_group_filter(group_filter)
        .with_version_filter(version_filter)
        .with_max_tests(max_tests)
        .with_verbose(verbose);

    let (_results, stats_by_group) = runner.run();

    // Print summary
    print_summary(&stats_by_group);
}

#[cfg(test)]
mod tests {
    

    #[test]
    fn test_stats_pass_rate() {
        let mut stats = TestStats::default();
        stats.passed = 80;
        stats.failed = 20;

        assert_eq!(stats.pass_rate(), 0.8);
    }

    #[test]
    fn test_stats_total() {
        let mut stats = TestStats::default();
        stats.passed = 10;
        stats.failed = 5;
        stats.skipped = 3;
        stats.errors = 2;

        assert_eq!(stats.total(), 20);
    }

    #[test]
    fn test_empty_stats() {
        let stats = TestStats::default();
        assert_eq!(stats.pass_rate(), 0.0);
        assert_eq!(stats.total(), 0);
    }
}
