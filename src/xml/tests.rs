//! Unit tests of the well-formedness layer: the reader on its own, below the
//! drivers (whose behaviour `tests/well_formedness.rs` covers end to end).
//!
//! Every case reads through both sources — the buffered [`WfReader`] and the
//! [`SliceWfReader`] — and checks that they agree event by event: kind,
//! content, start and end offsets, and the error.

use std::io::BufRead;

use quick_xml::events::Event;

use super::reader::{NextEvent, SliceWfReader, WfError, WfReader};
use super::{ParseLimit, ParseLimits, WellFormednessRule as R};

/// Everything a source hands out for its input: per event its content and
/// offsets, then the error (as `Debug`), if any. The DOCTYPE keyword is left
/// to the next read, as a caller that does not report the declaration does.
fn trace<'i, B: BufRead, W>(mut r: WfReader<B, W>) -> (Vec<String>, Result<(), WfError>)
where
    WfReader<B, W>: NextEvent<'i>,
{
    trace_of(&mut r)
}

/// [`trace`] of a reader that the caller keeps.
fn trace_of<'i, B: BufRead, W>(r: &mut WfReader<B, W>) -> (Vec<String>, Result<(), WfError>)
where
    WfReader<B, W>: NextEvent<'i>,
{
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match r.next_event(&mut buf) {
            Ok(Event::Eof) => return (out, Ok(())),
            Ok(event) => out.push(format!(
                "{event:?} {}..{} {}",
                r.event_start(),
                r.buffer_position(),
                r.attributes().len()
            )),
            Err(e) => return (out, Err(e)),
        }
    }
}

/// The two sources over `xml`, which must agree; the buffered one's result.
fn both(xml: &[u8], limits: &ParseLimits, trim: bool) -> (Vec<String>, Result<(), WfError>) {
    let buffered = trace(WfReader::new(xml, limits, trim));
    let slice = trace(SliceWfReader::from_slice(xml, limits, trim));
    assert_eq!(
        (&buffered.0, format!("{:?}", buffered.1)),
        (&slice.0, format!("{:?}", slice.1)),
        "buffered and slice differ for {:?} (trim {trim})",
        String::from_utf8_lossy(xml)
    );
    buffered
}

/// Every event kind of `xml` (as `Debug` names), or the error; both sources.
fn read_all(xml: &[u8], limits: &ParseLimits, trim: bool) -> Result<Vec<String>, WfError> {
    let (events, end) = both(xml, limits, trim);
    end?;
    Ok(events
        .iter()
        .map(|e| e[..e.find(['(', ' ']).unwrap_or(e.len())].to_string())
        .map(|k| match k.as_str() {
            "GeneralRef" => "Ref".to_string(),
            _ => k,
        })
        .collect())
}

fn rule_at(xml: &[u8], trim: bool) -> (R, u64) {
    match read_all(xml, &ParseLimits::none(), trim) {
        Err(WfError::NotWellFormed(e)) => (e.rule(), e.position()),
        other => panic!(
            "expected NotWellFormed for {:?}, got {other:?}",
            String::from_utf8_lossy(xml)
        ),
    }
}

#[test]
fn a_well_formed_document_reads_through() {
    let xml = b"<?xml version='1.0'?>\n<!DOCTYPE r>\n<!-- c --><r a='1' xmlns:p='urn:p' p:b='2'>t&amp;<![CDATA[x]]><?pi d?><e/></r>\n";
    let events = read_all(xml, &ParseLimits::none(), false).unwrap();
    assert_eq!(
        events,
        [
            "Decl", "Text", "DocType", "Text", "Comment", "Start", "Text", "Ref", "CData", "PI",
            "Empty", "End", "Text"
        ]
    );
}

#[test]
fn positions_are_input_offsets_with_trimming_too() {
    // With quick-xml skipping white space before markup (the schema reader),
    // a position still points at the offending byte.
    assert_eq!(rule_at(b"<r>\n   ab\x01</r>", true), (R::Char, 9));
    assert_eq!(rule_at(b"<r>\n   ab\x01</r>", false), (R::Char, 9));
    assert_eq!(
        rule_at(b"  <?xml version='1.0'?><r/>", true),
        (R::XmlDeclaration, 2)
    );
    assert_eq!(rule_at(b"<r/>\n  x", true), (R::Document, 7));
    assert_eq!(
        rule_at(b"<r/>  <!DOCTYPE r>", true).0,
        R::DocumentTypeDeclaration
    );
    assert_eq!(rule_at(b"\xEF\xBB\xBF<r>\x01</r>", false), (R::Char, 6));
}

#[test]
fn the_doctype_keyword_is_checked_on_the_next_read() {
    // The event does not show the keyword; the markup still in the buffer
    // does, and the next read looks at it.
    let mut r = WfReader::new(&b"<!doctype r><r/>"[..], &ParseLimits::none(), false);
    let mut buf = Vec::new();
    assert!(matches!(r.read_event_into(&mut buf), Ok(Event::DocType(_))));
    match r.read_event_into(&mut buf) {
        Err(WfError::NotWellFormed(e)) => {
            assert_eq!(e.rule(), R::DocumentTypeDeclaration);
            assert_eq!(e.position(), 0);
        }
        other => panic!("{other:?}"),
    }
    // The slice source looks at the input, at the next read or when asked.
    let mut r = SliceWfReader::from_slice(b"<!doctype r><r/>", &ParseLimits::none(), false);
    assert!(matches!(r.read_event(), Ok(Event::DocType(_))));
    match r.read_event() {
        Err(WfError::NotWellFormed(e)) => {
            assert_eq!(e.rule(), R::DocumentTypeDeclaration);
            assert_eq!(e.position(), 0);
        }
        other => panic!("{other:?}"),
    }
    for (xml, trim) in [
        (&b"\n <!doctype r><r/>"[..], true),
        (b"\n <!doctype r><r/>", false),
    ] {
        let mut b = WfReader::new(xml, &ParseLimits::none(), trim);
        let mut s = SliceWfReader::from_slice(xml, &ParseLimits::none(), trim);
        let mut buf = Vec::new();
        loop {
            let (x, y) = (b.read_event_into(&mut buf), s.read_event());
            assert_eq!(format!("{x:?}"), format!("{y:?}"));
            if matches!(x, Ok(Event::DocType(_))) {
                let one = NextEvent::confirm_doctype(&mut b, &buf);
                let two = NextEvent::confirm_doctype(&mut s, &[]);
                assert_eq!(format!("{one:?}"), format!("{two:?}"));
                assert!(two.is_err(), "'<!doctype', trim {trim}");
                break;
            }
            assert!(x.is_ok());
        }
    }
    for (xml, trim) in [
        (&b"  <!doctype r><r/>"[..], true),
        (b"\xEF\xBB\xBF<!DOCTYPE r><r/>", true),
        (b"\xEF\xBB\xBF\n<!DocType r><r/>", true),
        (b"\xEF\xBB\xBF\n<!DocType r><r/>", false),
    ] {
        let _ = both(xml, &ParseLimits::none(), trim);
        let mut b = WfReader::new(xml, &ParseLimits::none(), trim);
        let mut s = SliceWfReader::from_slice(xml, &ParseLimits::none(), trim);
        let mut buf = Vec::new();
        loop {
            let (x, y) = (b.read_event_into(&mut buf), s.read_event());
            if let Ok(Event::DocType(_)) = x {
                assert!(matches!(y, Ok(Event::DocType(_))));
                let one = NextEvent::confirm_doctype(&mut b, &buf);
                let two = NextEvent::confirm_doctype(&mut s, &[]);
                assert_eq!(
                    format!("{one:?}"),
                    format!("{two:?}"),
                    "{xml:?} trim {trim}"
                );
                break;
            }
            assert_eq!(format!("{x:?}"), format!("{y:?}"));
        }
    }
    // A caller that clears the buffer in between only loses that check.
    let mut r = WfReader::new(&b"<!doctype r><r/>"[..], &ParseLimits::none(), false);
    let mut buf = Vec::new();
    assert!(r.read_event_into(&mut buf).is_ok());
    buf.clear();
    assert!(matches!(r.read_event_into(&mut buf), Ok(Event::Empty(_))));
}

#[test]
fn truncation_and_a_missing_root_are_reported_at_the_end() {
    match read_all(b"<r><e>", &ParseLimits::none(), false) {
        Err(WfError::NotWellFormed(e)) => {
            assert_eq!(e.rule(), R::Truncated);
            assert_eq!(e.position(), 6);
            assert!(e.message().contains("2 element"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rule_at(b"<!-- c -->", false), (R::Document, 10));
    assert_eq!(rule_at(b"", false), (R::Document, 0));
}

#[test]
fn quick_xml_refusals_are_not_well_formed_too() {
    assert_eq!(rule_at(b"<r></s>", false), (R::ElementTypeMatch, 3));
    assert_eq!(rule_at(b"<r><!-- x", false).0, R::Truncated);
    assert_eq!(rule_at(b"<r>a & b</r>", false).0, R::Reference);
    assert_eq!(rule_at(b"<r a='1' a='2'/>", false), (R::UniqueAttSpec, 9));
}

#[test]
fn a_limit_is_checked_before_the_tag_is_returned() {
    let limits = ParseLimits::none().with_max_attributes(1);
    assert!(both(b"<r><e a='1' b='2'/></r>", &limits, false).1.is_err());
    let mut r = WfReader::new(&b"<r><e a='1' b='2'/></r>"[..], &limits, false);
    let mut buf = Vec::new();
    assert!(matches!(r.read_event_into(&mut buf), Ok(Event::Start(_))));
    match r.read_event_into(&mut buf) {
        Err(WfError::Limit(e)) => {
            assert_eq!(e.limit(), ParseLimit::Attributes);
            assert_eq!((e.max(), e.found(), e.position()), (1, 2, 12));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn text_runs_are_measured_across_references() {
    let limits = ParseLimits::none().with_max_text_bytes(6);
    assert!(read_all(b"<r>a&amp;</r>", &limits, false).is_ok());
    match read_all(b"<r>a&amp;b</r>", &limits, false) {
        Err(WfError::Limit(e)) => {
            assert_eq!(e.limit(), ParseLimit::TextSize);
            assert_eq!((e.found(), e.position()), (7, 3));
        }
        other => panic!("{other:?}"),
    }
    // A CDATA section, comment or PI is measured on its own.
    assert!(read_all(b"<r>abcdef<![CDATA[abcdef]]>abcdef</r>", &limits, false).is_ok());
}

#[test]
fn namespace_scope_follows_the_elements() {
    // `b:z` and `a:z` collide only while `b` is bound to `urn:u`.
    let ok = b"<r xmlns:a='urn:u'><s xmlns:b='urn:u'/><t xmlns:b='urn:v' a:z='1' b:z='2'/></r>";
    assert!(read_all(ok, &ParseLimits::none(), false).is_ok());
    let bad = b"<r xmlns:a='urn:u' xmlns:b='urn:u'><s><t a:z='1' b:z='2'/></s></r>";
    assert_eq!(rule_at(bad, false).0, R::AttributesUnique);
    // Many prefixed attributes take the hashed path.
    let mut many = String::from("<r xmlns:a='urn:u' xmlns:b='urn:v' xmlns:c='urn:u'");
    for i in 0..20 {
        many.push_str(&format!(" a:x{i}='1' b:x{i}='1'"));
    }
    let ok = format!("{many}/>");
    assert!(read_all(ok.as_bytes(), &ParseLimits::none(), false).is_ok());
    many.push_str(" c:x7='1'/>");
    assert_eq!(rule_at(many.as_bytes(), false).0, R::AttributesUnique);
}

#[test]
fn an_unrelated_buffer_after_a_doctype_is_not_mistaken_for_it() {
    // A caller that passes another buffer after the DOCTYPE read: the
    // keyword check looks only at the declaration's own markup.
    let mut r = WfReader::new(&b"<!DOCTYPE r><r/>"[..], &ParseLimits::none(), false);
    let mut buf = Vec::new();
    assert!(matches!(r.read_event_into(&mut buf), Ok(Event::DocType(_))));
    let mut other = b"<!doctype zz>".to_vec();
    assert!(matches!(r.read_event_into(&mut other), Ok(Event::Empty(_))));
}

#[test]
fn xml_1_1_positions_and_citations() {
    let xml = b"<?xml version='1.1'?><r>ab\xC2\x80</r>";
    match read_all(xml, &ParseLimits::none(), false) {
        Err(WfError::NotWellFormed(e)) => {
            assert_eq!((e.rule(), e.position()), (R::Char, 26));
            assert_eq!(e.xml_version(), super::XmlVersion::V1_1);
            assert!(
                e.to_string()
                    .contains("XML 1.1 §2.2 [2] Char, [2a] RestrictedChar"),
                "{e}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn bytes_that_are_not_utf8_are_refused_as_the_checks_refuse_them() {
    // quick-xml refuses an event whose bytes are not UTF-8 before the layer
    // checks it; the layer reports what its checks find in those bytes, at
    // the same rule and position whatever the source's chunks.
    let cases: &[(&[u8], R, u64)] = &[
        (b"<r>ab\xFF</r>", R::Encoding, 5),
        (b" \xFF<r/>", R::Document, 1),
        (b"<r>&a\xFF;</r>", R::Encoding, 4),
        (b"<r>&a\xFF</r>", R::Reference, 3),
        (b"<r a=\"\xFF\"/>", R::Encoding, 6),
        (b"<r a\xFF=\"1\"/>", R::Encoding, 4),
        (b"<r></r\xFF>", R::ElementTypeMatch, 3),
        (b"<r/></r\xFF>", R::ElementTypeMatch, 4),
        (b"<!--\xFF--><r/>", R::Encoding, 4),
        (b"<r><![CDATA[\xFF]]></r>", R::Encoding, 12),
        (b"<?pi \xFF?><r/>", R::Encoding, 5),
        (b"<?xml version=\"1.0\"\xFF?><r/>", R::XmlDeclaration, 19),
        (b"<!DOCTYPE r\xFF><r/>", R::Encoding, 11),
        (b"<!DOCTYX r\xFF><r/>", R::Truncated, 0),
        (b"<!-x\xFF--><r/>", R::Truncated, 0),
        (b"\xEF\xBB\xBF<r>\xC3</r>", R::Encoding, 6),
    ];
    for &(xml, rule, pos) in cases {
        assert_eq!(
            rule_at(xml, false),
            (rule, pos),
            "{:?}",
            String::from_utf8_lossy(xml)
        );
        // The same in one-byte chunks.
        let mut r = WfReader::new(
            std::io::BufReader::with_capacity(1, xml),
            &ParseLimits::none(),
            false,
        );
        let mut buf = Vec::new();
        let found = loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Eof) => panic!("{:?} read through", String::from_utf8_lossy(xml)),
                Ok(_) => {}
                Err(WfError::NotWellFormed(e)) => break (e.rule(), e.position()),
                Err(other) => panic!("{other:?}"),
            }
        };
        assert_eq!(
            found,
            (rule, pos),
            "{:?} in chunks",
            String::from_utf8_lossy(xml)
        );
        // With white space skipped before markup (the schema reader), and
        // after leading white space: the two sources still agree.
        assert!(both(xml, &ParseLimits::none(), true).1.is_err());
        let mut spaced = b" \n\t".to_vec();
        spaced.extend_from_slice(xml);
        let _ = both(&spaced, &ParseLimits::none(), true);
        let _ = both(&spaced, &ParseLimits::none(), false);
    }
}

/// What the slice source relies on for a refused event: quick-xml's slice
/// reader has moved past the refused bytes exactly as the buffered one has
/// (the same `buffer_position` and `error_position`), and those bytes — the
/// buffered reader's buffer — are the input between the offsets before and
/// after the read, a leading BOM uncounted. Re-run this first when quick-xml
/// changes how it validates UTF-8.
#[test]
fn quick_xml_moves_past_refused_bytes_alike_in_both_readers() {
    let cases: &[&[u8]] = &[
        b"<r>ab\xFF</r>",
        b" \xFF<r/>",
        b"<r>&a\xFF;</r>",
        b"<r>&a\xFF</r>",
        b"<r a=\"\xFF\"/>",
        b"<r></r\xFF>",
        b"<!--\xFF--><r/>",
        b"<r><![CDATA[\xFF]]></r>",
        b"<?pi \xFF?><r/>",
        b"<!DOCTYPE r\xFF><r/>",
        b"\xEF\xBB\xBF<r>\xC3</r>",
        b"\xEF\xBB\xBF \n<r a='\xC3'/>",
        b"<r>t&amp;\xFF</r>",
    ];
    for &xml in cases {
        for trim in [false, true] {
            let bom = if xml.starts_with(&[0xEF, 0xBB, 0xBF]) {
                3
            } else {
                0
            };
            let mut b = quick_xml::Reader::from_reader(xml);
            let mut s = quick_xml::Reader::from_reader(xml);
            b.config_mut().trim_text_start = trim;
            s.config_mut().trim_text_start = trim;
            let mut buf = Vec::new();
            loop {
                buf.clear();
                let before = s.buffer_position();
                assert_eq!(b.buffer_position(), before);
                let (x, y) = (b.read_event_into(&mut buf), s.read_event());
                match (x, y) {
                    (Ok(Event::Eof), _) => panic!("{xml:?} read through"),
                    (Ok(x), Ok(y)) => assert_eq!(format!("{x:?}"), format!("{y:?}")),
                    (Err(x), Err(y)) => {
                        assert!(matches!(x, quick_xml::Error::Encoding(_)), "{x:?}");
                        assert!(matches!(y, quick_xml::Error::Encoding(_)), "{y:?}");
                        let after = s.buffer_position();
                        assert_eq!(b.buffer_position(), after, "{xml:?}");
                        assert_eq!(b.error_position(), s.error_position(), "{xml:?}");
                        let read = &xml[(before + bom) as usize..(after + bom) as usize];
                        let skipped = if trim {
                            read.iter()
                                .position(|c| !b" \t\r\n".contains(c))
                                .unwrap_or(read.len())
                        } else {
                            0
                        };
                        assert_eq!(&read[skipped..], &buf[..], "{xml:?} trim {trim}");
                        break;
                    }
                    (x, y) => panic!("{xml:?}: {x:?} against {y:?}"),
                }
            }
        }
    }
}

/// A reader that works in the buffers of the readers before it reads as a
/// fresh one does, whatever those left: open elements and declarations of a
/// document that stopped half-way, an XML declaration, attributes.
#[cfg(feature = "plan")]
#[test]
fn the_buffers_of_one_reader_serve_the_next() {
    use super::reader::WfBuffers;
    let docs: [&[u8]; 7] = [
        b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\" standalone=\"yes\"?><r a=\"1\" b=\"2\"/>",
        b"<p:r xmlns:p=\"urn:p\" xmlns=\"urn:d\"><p:a xmlns:p=\"urn:q\" p:x=\"1\"><b xmlns=\"\"/></p:a><p:c/></p:r>",
        // Stops inside three elements, with declarations in force.
        b"<r xmlns=\"urn:d\" xmlns:p=\"urn:p\"><p:a><b xmlns:q=\"urn:q\" q:k=\"v\">text",
        // The prefixes of the documents before are not bound here.
        b"<r><p:a/></r>",
        b"<?xml version=\"1.1\"?><r>&#x1;</r>",
        b"<r/>",
        b"<r xmlns:p=\"urn:p\" p:a=\"1\" p:a=\"2\"/>",
    ];
    let limits = ParseLimits::default();
    let declaration = |r: &SliceWfReader<'_>| r.declaration().map(|d| d.to_declaration());
    let mut buffers = WfBuffers::default();
    for _ in 0..2 {
        for doc in docs {
            let mut fresh = SliceWfReader::from_slice(doc, &limits, false);
            let expected = trace_of(&mut fresh);
            let mut reusing = SliceWfReader::from_slice_reusing(doc, &limits, false, buffers);
            let found = trace_of(&mut reusing);
            assert_eq!(
                (&expected.0, format!("{:?}", expected.1)),
                (&found.0, format!("{:?}", found.1)),
                "{:?}",
                String::from_utf8_lossy(doc)
            );
            assert_eq!(declaration(&fresh), declaration(&reusing));
            assert_eq!(fresh.xml_version(), reusing.xml_version());
            buffers = reusing.into_buffers();
        }
    }
}

/// An error that names the innermost open element names it alike from both
/// sources: the buffered one copies the names of the open elements, the slice
/// one finds the name in the input when the error is built.
#[test]
fn the_innermost_open_element_is_named_alike_by_both_sources() {
    let cases: [(&[u8], &str); 11] = [
        (b"<r><p:a xmlns:p=\"u\"><b>text", "'b' (3 elements"),
        (b"<r><p:a xmlns:p=\"u\">", "'p:a' (2 elements"),
        (
            b"\xEF\xBB\xBF<r><a-long-element-name a=\"1\">",
            "'a-long-element-name' (2",
        ),
        (
            b"\xEF\xBB\xBF<?xml version=\"1.0\"?>\n<r>",
            "'r' (1 element ",
        ),
        // After a closed child the parent is the innermost again.
        (b"<r>\n  <a>\n    <b/><c></c>\n  ", "'a' (2 elements"),
        (b"<r><a></b></r>", "'<a>'"),
        (b"<r><a></\xFF></r>", "'<a>'"),
        (b"<r><a><!-- not closed", "'a' (2 elements"),
        (b"<r><a><?pi not closed", "'a' (2 elements"),
        (b"<r><a><b attr=\"not closed", "'a' (2 elements"),
        (
            "<\u{e9}:\u{fc} xmlns:\u{e9}=\"u\"><\u{f6}>".as_bytes(),
            "'\u{f6}' (2 elements",
        ),
    ];
    for (xml, named) in cases {
        for trim in [false, true] {
            let (_, end) = both(xml, &ParseLimits::default(), trim);
            let message = format!("{:?}", end.expect_err("refused"));
            assert!(
                message.contains(named),
                "{:?} (trim {trim}): {message}",
                String::from_utf8_lossy(xml)
            );
        }
    }
}
