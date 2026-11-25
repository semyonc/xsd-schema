#![cfg(feature = "xsd11")]
//! One current dateTime per validation run.
//!
//! XSD 1.1 Structures §3.13.4.2 (XPath Evaluation), clause 6: "The current
//! dateTime is ·implementation-defined·, but is constant during an
//! ·assessment· episode." A run evaluates every assertion — a complex type's
//! `xs:assert`, an `xs:assertion` facet on an attribute's or an element's
//! type — and every type alternative in a dynamic context of its own, and it
//! builds the assertion's data model, whose typed values (ids, a union's
//! whitespace) come from the same validation. All of them must agree on one
//! instant: the host's ([`ValidationRuntime::set_current_time`]), or the
//! system clock read the first time an expression of the run asks.
//!
//! Most tests give the run an instant of the host's, far from now, so that a
//! reading of the system clock anywhere in the run shows; none depends on how
//! fast this machine is. Where the system clock is read, a test only compares
//! instants that program order puts before or after each other.
//!
//! [`ValidationRuntime::set_current_time`]: xsd_schema::validation::ValidationRuntime::set_current_time

use std::cell::Cell;
use std::io::{BufReader, Read};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bumpalo::Bump;
use xsd_schema::document::{
    build_typed_document_with, BufferDocumentOptions, SilentValidationSink, TypedBuildOptions,
};
use xsd_schema::validation::{
    drive_navigator, drive_quick_xml, CollectingValidationSink, NoopHandler, SchemaValidator,
    ValidationError, ValidationFlags,
};
use xsd_schema::xpath::{api::XPathExpr, XPathContext};
use xsd_schema::{RoXmlNavigator, SchemaSet, SchemaSetBuilder, XsdVersion};

/// The test of the stamping assertions: fails with the instant in its message.
const STAMP: &str = "error((), concat('at=', string(current-dateTime()), ';'))";

/// The deadline of the time-dependent types.
const DEADLINE: &str = "2000-01-01T00:00:00Z";

fn compile(xsd: &str) -> SchemaSet {
    SchemaSetBuilder::with_version(XsdVersion::V1_1)
        .add_source(xsd, "file:///validation_run_clock.xsd")
        .expect("schema source accepted")
        .compile()
        .unwrap_or_else(|e| panic!("schema compiles: {e}"))
        .into_schema_set()
}

fn flags() -> ValidationFlags {
    ValidationFlags::default() | ValidationFlags::PROCESS_ASSERTIONS
}

/// Day number of a proleptic Gregorian date, 1970-01-01 being 0.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The instant `YYYY-MM-DDTHH:MM:SS(.fraction)?Z` (after 1970).
fn instant_of(stamp: &str) -> SystemTime {
    let (date, time) = stamp
        .strip_suffix('Z')
        .and_then(|s| s.split_once('T'))
        .unwrap_or_else(|| panic!("a UTC dateTime: {stamp}"));
    let date: Vec<i64> = date.split('-').map(|f| f.parse().unwrap()).collect();
    let time: Vec<&str> = time.split(':').collect();
    let (whole, fraction) = time[2].split_once('.').unwrap_or((time[2], ""));
    let nanos: u32 = format!("{fraction:0<9}")[..9].parse().unwrap();
    let seconds = days_from_civil(date[0], date[1], date[2]) * 86_400
        + time[0].parse::<i64>().unwrap() * 3_600
        + time[1].parse::<i64>().unwrap() * 60
        + whole.parse::<i64>().unwrap();
    UNIX_EPOCH + Duration::new(u64::try_from(seconds).unwrap(), nanos)
}

/// 1999-12-31T23:00:00Z, before the deadline, and 2000-01-01T01:00:00Z,
/// after it.
fn before_deadline() -> SystemTime {
    instant_of("1999-12-31T23:00:00Z")
}
fn after_deadline() -> SystemTime {
    instant_of("2000-01-01T01:00:00Z")
}

/// Gives the parts one after another, pausing before each one but the first,
/// and notes when the last pause began.
struct Slow<'a> {
    parts: std::collections::VecDeque<&'a [u8]>,
    pause: Duration,
    pause_next: bool,
    paused_at: Rc<Cell<Option<SystemTime>>>,
}

impl Read for Slow<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(part) = self.parts.pop_front() else {
            return Ok(0);
        };
        if self.pause_next {
            self.paused_at.set(Some(SystemTime::now()));
            std::thread::sleep(self.pause);
            self.pause_next = false;
        }
        let n = part.len().min(buf.len());
        buf[..n].copy_from_slice(&part[..n]);
        if n < part.len() {
            self.parts.push_front(&part[n..]);
        } else {
            self.pause_next = true;
        }
        Ok(n)
    }
}

/// What one streaming run reported.
struct Run {
    errors: Vec<ValidationError>,
    /// When the reader's last pause began, if it paused.
    paused_at: Option<SystemTime>,
}

/// Validate `parts`, read with `pause` between them, in a run whose current
/// dateTime is `now` (`None`: the system clock's).
fn stream(ss: &SchemaSet, parts: &[&[u8]], pause: Duration, now: Option<SystemTime>) -> Run {
    stream_with(ss, parts, pause, now, flags())
}

/// [`stream`] with the validator's `flags`.
fn stream_with(
    ss: &SchemaSet,
    parts: &[&[u8]],
    pause: Duration,
    now: Option<SystemTime>,
    flags: ValidationFlags,
) -> Run {
    let validator = SchemaValidator::new(ss, flags);
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let paused_at = Rc::new(Cell::new(None));
    {
        let mut runtime = validator.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        if let Some(now) = now {
            runtime.set_current_time(now);
        }
        let reader = Slow {
            parts: parts.iter().copied().collect(),
            pause,
            pause_next: false,
            paused_at: Rc::clone(&paused_at),
        };
        drive_quick_xml(
            BufReader::with_capacity(64 * 1024, reader),
            &mut runtime,
            ss,
        )
        .expect("drive");
    }
    Run {
        errors,
        paused_at: paused_at.get(),
    }
}

/// The errors of a run over the navigator driver.
fn navigate(ss: &SchemaSet, xml: &str, now: Option<SystemTime>) -> Vec<ValidationError> {
    let validator = SchemaValidator::new(ss, flags());
    let doc = roxmltree::Document::parse(xml).expect("well-formed");
    let nav = RoXmlNavigator::new(&doc);
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    {
        let mut runtime = validator.start_run(CollectingValidationSink {
            errors: &mut errors,
            warnings: &mut warnings,
        });
        if let Some(now) = now {
            runtime.set_current_time(now);
        }
        drive_navigator(&nav, &mut runtime, ss).expect("drive");
    }
    errors
}

/// The instants the stamping assertions of a run report, in order.
fn stamps(errors: &[ValidationError]) -> Vec<String> {
    errors
        .iter()
        .filter(|e| e.constraint == "cvc-assertion" && e.message.contains("at="))
        .map(|e| {
            // The message quotes the test (which contains `at=` too) before
            // the error's description: the description is the last `at=`.
            let at = e.message.rfind("at=").expect("the description") + 3;
            let end = e.message[at..].find(';').expect("the terminator") + at;
            e.message[at..end].to_string()
        })
        .collect()
}

/// Every error but the stamps, as `constraint: message`.
fn other_errors(errors: &[ValidationError]) -> Vec<String> {
    errors
        .iter()
        .filter(|e| !(e.constraint == "cvc-assertion" && e.message.contains("at=")))
        .map(|e| format!("{}: {}", e.constraint, e.message))
        .collect()
}

// ---------------------------------------------------------------------------
// Every expression of a run, one instant
// ---------------------------------------------------------------------------

/// A type alternative that reads the clock on the first element, then
/// stamping assertions of every kind: a complex type's, an attribute's
/// facet, an element's facet. `g` is valid only when its alternative sees the
/// instant `1980-06-01T12:00:00Z`.
fn stamping_schema() -> SchemaSet {
    compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Stamp">
    <xs:restriction base="xs:string"><xs:assertion test="{STAMP}"/></xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="Never">
    <xs:restriction base="xs:string"><xs:assertion test="false()"/></xs:restriction>
  </xs:simpleType>
  <xs:complexType name="A">
    <xs:simpleContent><xs:extension base="xs:string">
      <xs:attribute name="k" type="xs:string"/>
    </xs:extension></xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="B"><xs:simpleContent><xs:restriction base="A"/></xs:simpleContent></xs:complexType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="first" type="A">
      <xs:alternative test="seconds-from-dateTime(current-dateTime()) ge 0" type="B"/>
    </xs:element>
    <xs:element name="e" maxOccurs="unbounded"><xs:complexType><xs:simpleContent>
      <xs:extension base="xs:string">
        <xs:attribute name="s" type="Stamp"/>
        <xs:assert test="{STAMP}"/>
      </xs:extension>
    </xs:simpleContent></xs:complexType></xs:element>
    <xs:element name="v" type="Stamp" maxOccurs="unbounded"/>
    <xs:element name="g" type="xs:string" minOccurs="0">
      <xs:alternative test="current-dateTime() ne xs:dateTime('1980-06-01T12:00:00Z')" type="Never"/>
    </xs:element>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#
    ))
}

const FIRST: &[u8] = b"<doc><first k='1'>x</first>";
const REST: &[u8] = b"<e s='x'>t</e><e s='y'>t</e><v>x</v><v>y</v></doc>";

/// Two complex-type assertions, two attribute facets, two element facets.
const STAMPS: usize = 6;

fn assert_one_instant(what: &str, stamps: &[String]) -> String {
    assert_eq!(
        stamps.len(),
        STAMPS,
        "{what}: one error per stamping assertion"
    );
    let first = &stamps[0];
    assert!(first.ends_with('Z'), "{what}: {first} is in UTC");
    assert!(
        stamps.iter().all(|s| s == first),
        "{what}: the assertions of one run saw several instants: {stamps:?}"
    );
    first.clone()
}

/// The type alternative of the first element reads the clock — before the
/// reader pauses — and every assertion after the pause reports that instant.
#[test]
fn a_type_alternative_that_reads_the_clock_first_fixes_the_run_s_instant() {
    let ss = stamping_schema();
    let run = stream(&ss, &[FIRST, REST], Duration::from_millis(50), None);
    assert_eq!(other_errors(&run.errors), Vec::<String>::new());
    let instant = assert_one_instant("stream", &stamps(&run.errors));
    let paused_at = run.paused_at.expect("the reader paused");
    assert!(
        instant_of(&instant) <= paused_at,
        "the assertions after the pause report {instant}, read after the pause began"
    );
}

/// The navigator driver: one instant too.
#[test]
fn every_assertion_of_a_navigator_run_sees_one_instant() {
    let ss = stamping_schema();
    let xml = format!(
        "{}{}",
        std::str::from_utf8(FIRST).unwrap(),
        std::str::from_utf8(REST).unwrap()
    );
    let errors = navigate(&ss, &xml, None);
    assert_eq!(other_errors(&errors), Vec::<String>::new());
    assert_one_instant("navigator", &stamps(&errors));
}

/// Two runs, two instants: the instant belongs to the run, not to the schema
/// set or the validator.
#[test]
fn two_runs_have_two_instants() {
    let ss = stamping_schema();
    let one = stream(&ss, &[FIRST, REST], Duration::ZERO, None);
    std::thread::sleep(Duration::from_millis(50));
    let two = stream(&ss, &[FIRST, REST], Duration::ZERO, None);
    let (one, two) = (
        assert_one_instant("first run", &stamps(&one.errors)),
        assert_one_instant("second run", &stamps(&two.errors)),
    );
    assert!(
        instant_of(&one) < instant_of(&two),
        "two runs, one instant: {one}, {two}"
    );
}

/// The host's instant is what every expression of the run reports — the
/// assertions and the type alternative of `g`, which sees it — and nothing
/// reads the system clock.
#[test]
fn a_host_instant_is_every_expression_s() {
    let ss = stamping_schema();
    let host = instant_of("1980-06-01T12:00:00Z");
    let parts: &[&[u8]] = &[
        FIRST,
        b"<e s='x'>t</e><e s='y'>t</e><v>x</v><v>y</v><g>z</g></doc>",
    ];
    let run = stream(&ss, parts, Duration::from_millis(20), Some(host));
    assert_eq!(other_errors(&run.errors), Vec::<String>::new());
    assert_eq!(
        assert_one_instant("stream", &stamps(&run.errors)),
        "1980-06-01T12:00:00Z"
    );
    let xml =
        "<doc><first k='1'>x</first><e s='x'>t</e><e s='y'>t</e><v>x</v><v>y</v><g>z</g></doc>";
    let errors = navigate(&ss, xml, Some(host));
    assert_eq!(other_errors(&errors), Vec::<String>::new());
    assert_eq!(
        assert_one_instant("navigator", &stamps(&errors)),
        "1980-06-01T12:00:00Z"
    );
    // Without the host's instant, `g`'s alternative does not match.
    let errors = navigate(&ss, xml, None);
    assert!(
        other_errors(&errors).iter().any(|e| e.contains("false()")),
        "{:?}",
        other_errors(&errors)
    );
}

/// Runs on several threads at once each keep their own instant.
#[test]
fn concurrent_runs_keep_their_own_instants() {
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|i| {
                scope.spawn(move || {
                    // A schema set is not `Sync`: each thread has its own.
                    let ss = stamping_schema();
                    let host = instant_of(&format!("199{i}-03-0{}T10:00:00Z", i + 1));
                    let hosted = stream(&ss, &[FIRST, REST], Duration::from_millis(10), Some(host));
                    let hosted = assert_one_instant("hosted", &stamps(&hosted.errors));
                    assert_eq!(instant_of(&hosted), host, "thread {i}");
                    let own = stream(&ss, &[FIRST, REST], Duration::from_millis(10), None);
                    assert_one_instant("system clock", &stamps(&own.errors));
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("the thread's runs");
        }
    });
}

// ---------------------------------------------------------------------------
// The assertion's data model: no second validation with another clock
// ---------------------------------------------------------------------------

/// An `xs:ID` type that is valid only before the deadline, two elements that
/// carry such ids, and an assertion that looks both ids up in the assertion's
/// data model — whose id index must hold what the run accepted.
fn timed_id_schema(deadline: &str) -> SchemaSet {
    compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="TimedId"><xs:restriction base="xs:ID">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{deadline}')"/>
  </xs:restriction></xs:simpleType>
  <xs:element name="doc"><xs:complexType>
    <xs:sequence>
      <xs:element name="a" maxOccurs="unbounded"><xs:complexType>
        <xs:attribute name="id" type="TimedId"/>
      </xs:complexType></xs:element>
      <xs:element name="b" type="TimedId" minOccurs="0"/>
    </xs:sequence>
    <xs:assert test="exists(id('one')) and exists(id('foo'))"/>
  </xs:complexType></xs:element>
</xs:schema>"#
    ))
}

const IDS: [&[u8]; 2] = [b"<doc><a id='one'/>", b"<a id='foo'/></doc>"];

/// Whether the run's verdicts agree with each other: the assertion fails
/// exactly when an id was rejected, and the two ids have the same verdict.
fn consistent(errors: &[ValidationError]) -> Result<(), String> {
    let rejected = errors
        .iter()
        .filter(|e| e.message.contains("current-dateTime() lt"))
        .count();
    let lookup_failed = errors
        .iter()
        .any(|e| e.message.contains("exists(id('one'))"));
    let all: Vec<_> = errors.iter().map(|e| e.message.clone()).collect();
    if lookup_failed != (rejected > 0) {
        return Err(format!(
            "ids rejected: {rejected}, lookup failed: {lookup_failed}: {all:?}"
        ));
    }
    if rejected != 0 && rejected != 2 {
        return Err(format!("the two ids, two verdicts: {all:?}"));
    }
    Ok(())
}

/// The review's reproduction with the host's instant: a slow reader and a
/// fast one give the same verdicts, decided by the run's instant — the
/// assertion's data model files the ids the run accepted, it does not validate
/// them again (which would read the system clock, long after the deadline).
#[test]
fn a_slow_reader_and_a_fast_one_give_the_same_verdicts() {
    let ss = timed_id_schema(DEADLINE);
    for pause in [Duration::ZERO, Duration::from_millis(30)] {
        let early = stream(&ss, &IDS, pause, Some(before_deadline()));
        assert!(
            early.errors.is_empty(),
            "pause {pause:?}: {:?}",
            other_errors(&early.errors)
        );
        let late = stream(&ss, &IDS, pause, Some(after_deadline()));
        consistent(&late.errors).unwrap();
        assert!(
            late.errors
                .iter()
                .any(|e| e.message.contains("exists(id('one'))")),
            "pause {pause:?}: both ids rejected, the lookup fails"
        );
    }
    // An element whose own value is the id, too.
    let early = stream(
        &ss,
        &[b"<doc><a id='one'/>", b"<b>foo</b></doc>"],
        Duration::from_millis(30),
        Some(before_deadline()),
    );
    assert!(early.errors.is_empty(), "{:?}", other_errors(&early.errors));
}

/// The same with the system clock: the run reads it at the first id, before
/// the reader pauses past the deadline, and every verdict of the run follows
/// that instant. (On a machine too slow to reach the first id before the
/// deadline both ids are rejected and the lookup fails: consistent too.)
#[test]
fn a_slow_reader_does_not_split_the_run_s_instant() {
    let ss = timed_id_schema(&deadline_in(Duration::from_millis(300)));
    let run = stream(&ss, &IDS, Duration::from_millis(600), None);
    consistent(&run.errors).unwrap();
}

/// A deadline `ahead` of the system clock, as the body of the
/// `xs:dateTime('…')` the schemas above write it in: the epoch plus a
/// duration, rather than a calendar date.
fn deadline_in(ahead: Duration) -> String {
    let since = (SystemTime::now() + ahead)
        .duration_since(UNIX_EPOCH)
        .unwrap();
    format!(
        "1970-01-01T00:00:00Z') + xs:dayTimeDuration('PT{}.{:03}S",
        since.as_secs(),
        since.subsec_millis()
    )
}

/// A union whose first member accepts a value only before the deadline and
/// keeps its spaces, the second (`xs:token`) collapses them: the value's
/// [schema normalized value] in the assertion's data model is the one of the
/// member the run took, and so is its typed value there (`fn:data`, which the
/// data model computes by validating the value again — with the run's
/// instant) — the assertion checks both against the run's instant.
fn timed_union_schema(deadline: &str) -> SchemaSet {
    compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Early"><xs:restriction base="xs:string">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{deadline}')"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="Early xs:token"/></xs:simpleType>
  <xs:element name="doc"><xs:complexType>
    <xs:sequence><xs:element name="v" type="U"/></xs:sequence>
    <xs:attribute name="u" type="U"/>
    <xs:assert test="if (current-dateTime() lt xs:dateTime('{deadline}'))
                     then (string(@u) eq '  a  b  ' and string(v) eq '  a  b  '
                           and not(data(@u) instance of xs:token)
                           and not(data(v) instance of xs:token))
                     else (string(@u) eq 'a b' and string(v) eq 'a b'
                           and data(@u) instance of xs:token
                           and data(v) instance of xs:token)"/>
  </xs:complexType></xs:element>
</xs:schema>"#
    ))
}

const UNION: [&[u8]; 2] = [b"<doc u='  a  b  '>", b"<v>  a  b  </v></doc>"];

#[test]
fn a_union_s_whitespace_is_the_member_s_the_run_took() {
    let ss = timed_union_schema(DEADLINE);
    for now in [before_deadline(), after_deadline()] {
        for pause in [Duration::ZERO, Duration::from_millis(30)] {
            // Without PSVI retention too: the run keeps no normalized value
            // then, and the member comes from its typed value.
            for flags in [flags(), flags() & !ValidationFlags::BUILD_PSVI_TYPED_VALUES] {
                let run = stream_with(&ss, &UNION, pause, Some(now), flags);
                assert_eq!(
                    other_errors(&run.errors),
                    Vec::<String>::new(),
                    "{now:?}, pause {pause:?}, {flags:?}"
                );
            }
        }
        let errors = navigate(&ss, "<doc u='  a  b  '><v>  a  b  </v></doc>", Some(now));
        assert_eq!(
            other_errors(&errors),
            Vec::<String>::new(),
            "navigator {now:?}"
        );
    }
}

/// The same with the system clock and a pause past the deadline between the
/// attribute and the element.
#[test]
fn a_slow_reader_does_not_split_a_union_s_member() {
    let ss = timed_union_schema(&deadline_in(Duration::from_millis(300)));
    let run = stream(&ss, &UNION, Duration::from_millis(600), None);
    assert_eq!(other_errors(&run.errors), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// The typed document of a run
// ---------------------------------------------------------------------------

/// A typed build files the ids the run accepted, and gives a union attribute
/// the whitespace of the member the run took — whatever the system clock says.
#[test]
fn a_typed_build_follows_the_run_s_instant() {
    let ss = timed_id_schema(DEADLINE);
    let validator = SchemaValidator::new(&ss, flags());
    let mut runtime = validator.start_run(SilentValidationSink);
    runtime.set_current_time(before_deadline());
    let arena = Bump::new();
    let built = build_typed_document_with(
        "<doc><a id='one'/><a id='foo'/></doc>".as_bytes(),
        &arena,
        &ss,
        &mut runtime,
        &mut NoopHandler,
        TypedBuildOptions::default().with_document_options(BufferDocumentOptions::full()),
    )
    .expect("the build completes");
    assert!(
        built.document.get_element_by_id("one").is_some()
            && built.document.get_element_by_id("foo").is_some(),
        "the typed document's id index holds the ids the run accepted"
    );

    let ss = timed_union_schema(DEADLINE);
    let validator = SchemaValidator::new(&ss, flags());
    let mut runtime = validator.start_run(SilentValidationSink);
    runtime.set_current_time(before_deadline());
    let arena = Bump::new();
    let built = build_typed_document_with(
        "<doc u='  a  b  '><v>x</v></doc>".as_bytes(),
        &arena,
        &ss,
        &mut runtime,
        &mut NoopHandler,
        TypedBuildOptions::default().with_document_options(BufferDocumentOptions::full()),
    )
    .expect("the build completes");
    let names = &ss.name_table;
    let ctx = XPathContext::new(names);
    let expr = XPathExpr::compile("string(/doc/@u)", &ctx).unwrap();
    let value = expr
        .evaluator(&ctx)
        .run_with_node(built.document.create_navigator())
        .unwrap();
    assert_eq!(value.to_owned_string().as_deref(), Some("  a  b  "));
}

// ---------------------------------------------------------------------------
// The review's further findings
// ---------------------------------------------------------------------------

/// The typed values of the nodes `fn:id` and `fn:idref` return, and of nodes
/// reached from them by other axes, are computed with the run's instant: an
/// `xs:ID` and an `xs:IDREF` type valid only before the deadline, the host's
/// instant before it.
#[test]
fn id_and_idref_lookups_keep_the_run_s_instant() {
    let asserts = [
        "data(a/@v) instance of xs:ID",
        "data(id('k')/@v) instance of xs:ID",
        "data(r/@to) instance of xs:IDREF",
        "data(idref('k')) instance of xs:IDREF",
        "data(id('k')/following-sibling::r/@to) instance of xs:IDREF",
        "data(id('k')/../r/@to) instance of xs:IDREF",
        "data(idref('k')/../preceding-sibling::a/@v) instance of xs:ID",
        "data(idref('k')/ancestor::doc/b) instance of xs:ID",
        "data(id('j')) instance of xs:ID",
        "data(id('j')/preceding-sibling::r/@to) instance of xs:IDREF",
    ]
    .iter()
    .map(|test| format!("<xs:assert test=\"{test}\"/>"))
    .collect::<String>();
    let ss = compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="TimedId"><xs:restriction base="xs:ID">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{DEADLINE}')"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="TimedRef"><xs:restriction base="xs:IDREF">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{DEADLINE}')"/>
  </xs:restriction></xs:simpleType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="a"><xs:complexType><xs:attribute name="v" type="TimedId"/></xs:complexType></xs:element>
    <xs:element name="r"><xs:complexType><xs:attribute name="to" type="TimedRef"/></xs:complexType></xs:element>
    <xs:element name="b" type="TimedId"/>
  </xs:sequence>{asserts}</xs:complexType></xs:element>
</xs:schema>"#
    ));
    let xml = "<doc><a v='k'/><r to='k'/><b>j</b></doc>";
    let run = stream(
        &ss,
        &[xml.as_bytes()],
        Duration::ZERO,
        Some(before_deadline()),
    );
    assert_eq!(other_errors(&run.errors), Vec::<String>::new());
    let errors = navigate(&ss, xml, Some(before_deadline()));
    assert_eq!(other_errors(&errors), Vec::<String>::new());
}

/// An enumeration of a union, of a simple type and of a simple content
/// restriction: its values are the schema set's, read once. The same schema
/// set, the same document and the same instant give the same verdict before
/// and after a wall-clock deadline (whichever it is: both outcomes are
/// accepted when they agree).
#[test]
fn an_enumeration_s_values_do_not_follow_the_wall_clock() {
    let deadline = deadline_in(Duration::from_millis(300));
    let ss = compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Early"><xs:restriction base="xs:string">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{deadline}')"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="Early xs:token"/></xs:simpleType>
  <xs:simpleType name="E"><xs:restriction base="U"><xs:enumeration value="  a  b  "/></xs:restriction></xs:simpleType>
  <xs:complexType name="Base"><xs:simpleContent><xs:extension base="U"/></xs:simpleContent></xs:complexType>
  <xs:complexType name="R"><xs:simpleContent><xs:restriction base="Base">
    <xs:enumeration value="  a  b  "/>
  </xs:restriction></xs:simpleContent></xs:complexType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="e" type="E"/><xs:element name="r" type="R"/>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#
    ));
    let host = SystemTime::now() - Duration::from_secs(3_600);
    let xml: &[u8] = b"<doc><e>  a  b  </e><r>  a  b  </r></doc>";
    let codes =
        |run: Run| -> Vec<&'static str> { run.errors.iter().map(|e| e.constraint).collect() };
    let before = codes(stream(&ss, &[xml], Duration::ZERO, Some(host)));
    std::thread::sleep(Duration::from_millis(600));
    let after = codes(stream(&ss, &[xml], Duration::ZERO, Some(host)));
    assert_eq!(
        before, after,
        "the wall clock changed an enumeration's values"
    );
}

/// A `fixed` value has one {value}: the declared type's path and an
/// `xsi:type`'s give the same verdict at every run instant.
#[test]
fn both_fixed_value_paths_read_one_value() {
    let ss = compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Early"><xs:restriction base="xs:string">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{DEADLINE}')"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="Early xs:token"/></xs:simpleType>
  <xs:simpleType name="U2"><xs:restriction base="U"/></xs:simpleType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="f" type="U" fixed="  a  b  "/>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#
    ));
    for now in [before_deadline(), after_deadline()] {
        for text in ["a b", "  a  b  "] {
            let declared = format!("<doc><f>{text}</f></doc>");
            let xsi = format!(
                r#"<doc xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><f xsi:type="U2">{text}</f></doc>"#
            );
            let fixed = |xml: &str| {
                stream(&ss, &[xml.as_bytes()], Duration::ZERO, Some(now))
                    .errors
                    .iter()
                    .any(|e| e.constraint == "cvc-elt.5.2.2")
            };
            assert_eq!(fixed(&declared), fixed(&xsi), "{now:?}, {text:?}");
        }
    }
}

/// A typed build holds what its run found, whatever the retention flags say:
/// a union element's text is the one its member gives, an element that is an
/// id by its type is filed, and a defaulted union attribute has one value.
#[test]
fn a_typed_build_holds_the_run_s_values_whatever_the_flags() {
    let ss = compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Early"><xs:restriction base="xs:string">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{DEADLINE}')"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="Early xs:token"/></xs:simpleType>
  <xs:simpleType name="TimedId"><xs:restriction base="xs:ID">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{DEADLINE}')"/>
  </xs:restriction></xs:simpleType>
  <xs:element name="doc"><xs:complexType>
    <xs:sequence><xs:element name="v" type="U"/><xs:element name="i" type="TimedId"/></xs:sequence>
    <xs:attribute name="d" type="U" default="  a  b  "/>
  </xs:complexType></xs:element>
</xs:schema>"#
    ));
    let mut defaults = Vec::new();
    for flags in [flags(), flags() & !ValidationFlags::BUILD_PSVI_TYPED_VALUES] {
        let validator = SchemaValidator::new(&ss, flags);
        let mut runtime = validator.start_run(SilentValidationSink);
        runtime.set_current_time(before_deadline());
        let arena = Bump::new();
        let built = build_typed_document_with(
            "<doc><v>  a  b  </v><i>k</i></doc>".as_bytes(),
            &arena,
            &ss,
            &mut runtime,
            &mut NoopHandler,
            TypedBuildOptions::default().with_document_options(BufferDocumentOptions::full()),
        )
        .expect("the build completes");
        let ctx = XPathContext::new(&ss.name_table);
        let eval = |xpath: &str| {
            XPathExpr::compile(xpath, &ctx)
                .unwrap()
                .evaluator(&ctx)
                .run_with_node(built.document.create_navigator())
                .unwrap()
                .to_owned_string()
        };
        assert_eq!(
            eval("string(/doc/v)").as_deref(),
            Some("  a  b  "),
            "{flags:?}"
        );
        assert!(built.document.get_element_by_id("k").is_some(), "{flags:?}");
        defaults.push(eval("string(/doc/@d)"));
    }
    assert_eq!(
        defaults[0], defaults[1],
        "a retention flag changed a defaulted attribute"
    );
}

// ---------------------------------------------------------------------------
// The third review: one value per value constraint, one document per run
// ---------------------------------------------------------------------------

/// What a typed build of `xml` holds at the paths `xpaths`, and whether the
/// id `k` is filed, through `build_typed_document_with` and through a
/// `TypedBuilderHandler` driven with `drive_quick_xml_with` — the two public
/// ways to build one.
fn typed_builds(
    ss: &SchemaSet,
    xml: &str,
    flags: ValidationFlags,
    now: SystemTime,
    xpaths: &[&str],
) -> [(Vec<String>, bool); 2] {
    use xsd_schema::document::{BufferDocument, BufferDocumentBuilder, TypedBuilderHandler};
    use xsd_schema::validation::drive_quick_xml_with;
    let ctx = XPathContext::new(&ss.name_table);
    let read = |doc: &BufferDocument<'_>| {
        let values = xpaths
            .iter()
            .map(|xpath| {
                XPathExpr::compile(xpath, &ctx)
                    .unwrap()
                    .evaluator(&ctx)
                    .run_with_node(doc.create_navigator())
                    .unwrap()
                    .to_owned_string()
                    .unwrap_or_default()
            })
            .collect();
        (values, doc.get_element_by_id("k").is_some())
    };
    let validator = SchemaValidator::new(ss, flags);
    let options = BufferDocumentOptions::full();

    let arena = Bump::new();
    let mut runtime = validator.start_run(SilentValidationSink);
    runtime.set_current_time(now);
    let built = build_typed_document_with(
        xml.as_bytes(),
        &arena,
        ss,
        &mut runtime,
        &mut NoopHandler,
        TypedBuildOptions::default().with_document_options(options),
    )
    .expect("the build completes");
    let wrapper = read(&built.document);

    let arena = Bump::new();
    let mut builder =
        BufferDocumentBuilder::new(&arena, &ss.name_table, Some(ss), options).expect("a builder");
    let mut runtime = validator.start_run(SilentValidationSink);
    runtime.set_current_time(now);
    {
        let mut handler = TypedBuilderHandler::new(&mut builder);
        drive_quick_xml_with(xml.as_bytes(), &mut runtime, ss, &mut handler)
            .unwrap_or_else(|_| panic!("the drive completes"));
    }
    let direct = read(&builder.finalize().expect("the document"));
    [wrapper, direct]
}

/// An empty element takes the default its schema reports — the {lexical
/// form} the schema set recorded (Structures §3.3.5.4 clause 1.1: "the
/// {lexical form} of the {value constraint}") — as the defaulted attribute of
/// the same type and literal does, and the document is the same whichever
/// public entry point drives the builder, with either flag set: the element's
/// text, the attribute's value, and the id the run accepted. The type is a
/// union whose first member is valid only before the deadline; the schema set
/// reads its literal at its own instant (the system clock's: an `xs:token`
/// today, `a b`), the run before the deadline. Whatever the schema set read,
/// the documents hold it.
#[test]
fn an_empty_element_takes_the_default_its_schema_reports() {
    let ss = compile(&format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Early"><xs:restriction base="xs:string">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{DEADLINE}')"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="Early xs:token"/></xs:simpleType>
  <xs:simpleType name="TimedId"><xs:restriction base="xs:ID">
    <xs:assertion test="current-dateTime() lt xs:dateTime('{DEADLINE}')"/>
  </xs:restriction></xs:simpleType>
  <xs:element name="r" type="U" default="  a  b  "/>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element ref="r"/>
    <xs:element name="i" type="TimedId"/>
  </xs:sequence>
  <xs:attribute name="d" type="U" default="  a  b  "/>
  </xs:complexType></xs:element>
</xs:schema>"#
    ));
    let declared = ss
        .lookup_element(None, ss.name_table.add("r"))
        .and_then(|r| ss.element_value_constraint(r))
        .expect("the value constraint of r")
        .lexical_form;
    assert!(
        declared == "a b" || declared == "  a  b  ",
        "the token's form or the string's: {declared:?}"
    );

    let xpaths = ["string(/doc/r)", "string(/doc/@d)"];
    for flags in [flags(), flags() & !ValidationFlags::BUILD_PSVI_TYPED_VALUES] {
        let [wrapper, direct] = typed_builds(
            &ss,
            "<doc><r/><i>k</i></doc>",
            flags,
            before_deadline(),
            &xpaths,
        );
        let expected = (vec![declared.clone(), declared.clone()], true);
        assert_eq!(wrapper, expected, "build_typed_document_with, {flags:?}");
        assert_eq!(direct, expected, "a handler driven directly, {flags:?}");
    }
    // An element with content keeps the whitespace of the member the run
    // took, on both entry points.
    let [wrapper, direct] = typed_builds(
        &ss,
        "<doc><r>  a  b  </r><i>k</i></doc>",
        flags(),
        before_deadline(),
        &xpaths[..1],
    );
    assert_eq!(wrapper, (vec!["  a  b  ".to_string()], true));
    assert_eq!(direct, wrapper);
}

/// The ends a handler is told, with what the run lent it.
#[derive(Default)]
struct Ends {
    asks: bool,
    plain: usize,
    with_values: Vec<(Option<String>, Option<String>)>,
    defaults_typed: Vec<bool>,
}

impl xsd_schema::validation::ValidationEventHandler for Ends {
    type Error = std::convert::Infallible;

    fn wants_typed_values(&self) -> bool {
        self.asks
    }
    fn after_end_element(
        &mut self,
        _info: &xsd_schema::validation::EndElementInfo,
        _depth: usize,
    ) -> Result<(), Self::Error> {
        self.plain += 1;
        Ok(())
    }
    fn after_end_element_values(
        &mut self,
        _info: &xsd_schema::validation::EndElementInfo,
        values: xsd_schema::validation::EndElementValues<'_>,
        _depth: usize,
    ) -> Result<(), Self::Error> {
        self.with_values.push((
            values.typed_value.map(|v| v.to_string_value()),
            values.normalized_value.map(str::to_owned),
        ));
        Ok(())
    }
    fn after_end_of_attributes(
        &mut self,
        view: xsd_schema::validation::EndOfAttributesView<'_>,
    ) -> Result<(), Self::Error> {
        self.defaults_typed.extend(
            view.default_attributes
                .iter()
                .map(|d| d.typed_value.is_some()),
        );
        Ok(())
    }
}

/// The element-end hook: a handler that asks (`wants_typed_values`) is lent
/// each element's typed value and [schema normalized value] and gets the
/// typed values of defaulted attributes, whatever `BUILD_PSVI_TYPED_VALUES`
/// says; one that does not ask gets the verdict-only end, as before; the
/// errors are the same either way; and a `TypedBuilderHandler` passes the
/// request of its inner handler on.
#[test]
fn a_handler_that_asks_is_lent_the_run_s_values() {
    use xsd_schema::document::{BufferDocumentBuilder, TypedBuilderHandler};
    use xsd_schema::validation::drive_quick_xml_with;
    let ss = compile(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="U"><xs:union memberTypes="xs:int xs:token"/></xs:simpleType>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="u" type="U"/>
    <xs:element name="d" type="U" default="  a  b  "/>
    <xs:element name="bad" type="xs:int"/>
  </xs:sequence>
  <xs:attribute name="da" type="xs:token" default="  x  y  "/>
  </xs:complexType></xs:element>
</xs:schema>"#,
    );
    let xml = "<doc><u> 7 </u><d/><bad>x</bad></doc>";
    let lexical = flags() & !ValidationFlags::BUILD_PSVI_TYPED_VALUES;
    let drive = |asks: bool, flags: ValidationFlags| {
        let validator = SchemaValidator::new(&ss, flags);
        let (mut errors, mut warnings) = (Vec::new(), Vec::new());
        let mut ends = Ends {
            asks,
            ..Ends::default()
        };
        {
            let mut runtime = validator.start_run(CollectingValidationSink {
                errors: &mut errors,
                warnings: &mut warnings,
            });
            drive_quick_xml_with(xml.as_bytes(), &mut runtime, &ss, &mut ends)
                .unwrap_or_else(|_| panic!("the drive completes"));
        }
        let errors: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
        (ends, errors)
    };
    for flags in [flags(), lexical] {
        let (plain, plain_errors) = drive(false, flags);
        assert_eq!(plain.plain, 4, "u, d, bad and doc end without values");
        assert!(plain.with_values.is_empty());
        // Without the request a defaulted attribute has its typed value only
        // when the run retains typed values.
        assert_eq!(
            plain.defaults_typed,
            [flags.contains(ValidationFlags::BUILD_PSVI_TYPED_VALUES)]
        );

        let (asked, asked_errors) = drive(true, flags);
        assert_eq!(asked.plain, 0, "the values hook replaces the plain one");
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            asked.with_values,
            [
                // A union's value and the normalized value its member gave.
                (some("7"), some("7")),
                // The default taken: the constraint's {lexical form}.
                (some("a b"), some("a b")),
                // Not valid: no value.
                (None, None),
                // No simple content: no value.
                (None, None),
            ],
            "{flags:?}"
        );
        assert_eq!(asked.defaults_typed, [true], "{flags:?}");
        assert_eq!(asked_errors, plain_errors);
        assert_eq!(asked_errors.len(), 1, "{asked_errors:?}");
    }

    // Through a `TypedBuilderHandler`, an inner handler is served as it asks.
    for asks in [false, true] {
        let validator = SchemaValidator::new(&ss, lexical);
        let arena = Bump::new();
        let mut builder = BufferDocumentBuilder::new(
            &arena,
            &ss.name_table,
            Some(&ss),
            BufferDocumentOptions::full(),
        )
        .expect("a builder");
        let mut runtime = validator.start_run(SilentValidationSink);
        let mut handler = TypedBuilderHandler::with_handler(
            &mut builder,
            Ends {
                asks,
                ..Ends::default()
            },
        );
        drive_quick_xml_with(xml.as_bytes(), &mut runtime, &ss, &mut handler)
            .unwrap_or_else(|_| panic!("the drive completes"));
        let inner = handler.handler();
        assert_eq!(inner.plain, if asks { 0 } else { 4 });
        assert_eq!(inner.with_values.len(), if asks { 4 } else { 0 });
        // The builder asked for the defaulted attribute's typed value.
        assert_eq!(inner.defaults_typed, [true]);
    }
}

/// A fixed value of a type that reads the current dateTime is compared by
/// value, the literal read with the bindings of the declaration it is written
/// on whichever member the instance selected: a union of a QName enumeration
/// (`{urn:a}k`) and a string type whose assertion reads the clock (and holds
/// at every instant), fixed to
/// `q:k` where `q` is `urn:b` — the string `q:k`. A document that binds `q`
/// to `urn:b` or `urn:c` writes the same string and is valid at all four
/// attribute sites and on the element; one that binds it to `urn:a` writes
/// the QName, another value, and is told so at each.
#[test]
fn a_time_dependent_fixed_value_is_compared_with_its_declaration_s_bindings() {
    let ss = compile(
        r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:q="urn:a">
  <xs:simpleType name="QE"><xs:restriction base="xs:QName"><xs:enumeration value="q:k"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Timed"><xs:restriction base="xs:string">
    <xs:assertion test="current-dateTime() eq current-dateTime()"/>
  </xs:restriction></xs:simpleType>
  <xs:simpleType name="U"><xs:union memberTypes="QE Timed"/></xs:simpleType>
  <xs:attribute name="g" type="U" fixed="q:k" xmlns:q="urn:b"/>
  <xs:attribute name="h" type="U"/>
  <xs:attribute name="w" type="U" fixed="q:k" xmlns:q="urn:b"/>
  <xs:element name="doc"><xs:complexType><xs:sequence>
    <xs:element name="e" type="U" fixed="q:k" xmlns:q="urn:b" minOccurs="0"/>
  </xs:sequence>
  <xs:attribute name="a" type="U" fixed="q:k" xmlns:q="urn:b"/>
  <xs:attribute ref="g"/>
  <xs:attribute ref="h" fixed="q:k" xmlns:q="urn:b"/>
  <xs:anyAttribute namespace="##local" processContents="strict"/>
  </xs:complexType></xs:element>
</xs:schema>"###,
    );
    let codes = |q_ns: &str, now: SystemTime| -> Vec<&'static str> {
        let xml =
            format!(r#"<doc xmlns:q="{q_ns}" a="q:k" g="q:k" h="q:k" w="q:k"><e>q:k</e></doc>"#);
        let errors = stream(&ss, &[xml.as_bytes()], Duration::ZERO, Some(now)).errors;
        errors.iter().map(|e| e.constraint).collect()
    };
    for now in [before_deadline(), after_deadline()] {
        assert_eq!(codes("urn:b", now), [""; 0]);
        assert_eq!(codes("urn:c", now), [""; 0]);
        let other = codes("urn:a", now);
        assert_eq!(
            other.iter().filter(|c| **c == "cvc-attribute.4").count()
                + other.iter().filter(|c| **c == "cvc-au").count(),
            4,
            "{other:?}"
        );
        assert_eq!(
            other.iter().filter(|c| **c == "cvc-elt.5.2.2").count(),
            1,
            "{other:?}"
        );
        assert_eq!(other.len(), 5, "{other:?}");
    }
}
