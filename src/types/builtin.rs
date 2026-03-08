//! Built-in type registry for XSD types
//!
//! This module provides the `BuiltinTypes` struct which contains pre-allocated
//! `SimpleTypeKey` references for all 50 built-in XSD simple types, plus
//! the built-in complex `xs:anyType`.
//!
//! ## Usage
//!
//! Built-in types are initialized when a `SchemaSet` is created and can be
//! accessed via `schema_set.builtin_types()`.
//!
//! ```rust
//! use xsd_schema::SchemaSet;
//!
//! let schema_set = SchemaSet::new();
//! let string_key = schema_set.builtin_types().string;
//! ```

use std::collections::HashMap;

use crate::ids::{ComplexTypeKey, NameId, SimpleTypeKey, TypeKey};
use crate::namespace::table::well_known;
use crate::schema::model::{SchemaSet, XsdVersion};
use crate::arenas::{ComplexTypeDefData, SimpleTypeDefData};
use crate::parser::frames::{
    ComplexContentDefResult, ComplexContentResult, DerivationMethod, ParticleResult, ParticleTerm,
    ProcessContents, WildcardNamespace, WildcardResult,
};
use super::{XmlTypeCode, BuiltInType};

/// Well-known built-in type IDs for fast access.
///
/// This struct contains `SimpleTypeKey` references for all 50 built-in XSD
/// simple types. It is initialized when a `SchemaSet` is created.
///
/// Types are organized by category:
/// - Abstract types (anySimpleType, anyAtomicType)
/// - String types (string, normalizedString, token, etc.)
/// - Numeric types (boolean, decimal, float, double, integer hierarchy)
/// - Date/time types (duration, dateTime, date, time, gregorian types)
/// - Binary types (hexBinary, base64Binary)
/// - Other types (anyURI, QName, NOTATION)
/// - List types (NMTOKENS, IDREFS, ENTITIES)
#[derive(Debug, Clone)]
pub struct BuiltinTypes {
    // Complex types
    /// xs:anyType - the ur-type
    pub any_type: ComplexTypeKey,

    // Abstract types
    /// xs:anySimpleType - base of all simple types
    pub any_simple_type: SimpleTypeKey,
    /// xs:anyAtomicType - base of all atomic types (XSD 1.1)
    pub any_atomic_type: Option<SimpleTypeKey>,

    // String types
    /// xs:string
    pub string: SimpleTypeKey,
    /// xs:normalizedString
    pub normalized_string: SimpleTypeKey,
    /// xs:token
    pub token: SimpleTypeKey,
    /// xs:language
    pub language: SimpleTypeKey,
    /// xs:NMTOKEN
    pub nmtoken: SimpleTypeKey,
    /// xs:Name
    pub name: SimpleTypeKey,
    /// xs:NCName
    pub ncname: SimpleTypeKey,
    /// xs:ID
    pub id: SimpleTypeKey,
    /// xs:IDREF
    pub idref: SimpleTypeKey,
    /// xs:ENTITY
    pub entity: SimpleTypeKey,

    // Numeric types
    /// xs:boolean
    pub boolean: SimpleTypeKey,
    /// xs:decimal
    pub decimal: SimpleTypeKey,
    /// xs:float
    pub float: SimpleTypeKey,
    /// xs:double
    pub double: SimpleTypeKey,
    /// xs:integer
    pub integer: SimpleTypeKey,
    /// xs:nonPositiveInteger
    pub non_positive_integer: SimpleTypeKey,
    /// xs:negativeInteger
    pub negative_integer: SimpleTypeKey,
    /// xs:long
    pub long: SimpleTypeKey,
    /// xs:int
    pub int: SimpleTypeKey,
    /// xs:short
    pub short: SimpleTypeKey,
    /// xs:byte
    pub byte: SimpleTypeKey,
    /// xs:nonNegativeInteger
    pub non_negative_integer: SimpleTypeKey,
    /// xs:unsignedLong
    pub unsigned_long: SimpleTypeKey,
    /// xs:unsignedInt
    pub unsigned_int: SimpleTypeKey,
    /// xs:unsignedShort
    pub unsigned_short: SimpleTypeKey,
    /// xs:unsignedByte
    pub unsigned_byte: SimpleTypeKey,
    /// xs:positiveInteger
    pub positive_integer: SimpleTypeKey,

    // Date/time types
    /// xs:duration
    pub duration: SimpleTypeKey,
    /// xs:dateTime
    pub datetime: SimpleTypeKey,
    /// xs:time
    pub time: SimpleTypeKey,
    /// xs:date
    pub date: SimpleTypeKey,
    /// xs:gYearMonth
    pub g_year_month: SimpleTypeKey,
    /// xs:gYear
    pub g_year: SimpleTypeKey,
    /// xs:gMonthDay
    pub g_month_day: SimpleTypeKey,
    /// xs:gDay
    pub g_day: SimpleTypeKey,
    /// xs:gMonth
    pub g_month: SimpleTypeKey,

    // XSD 1.1 date/time additions
    /// xs:yearMonthDuration (XSD 1.1)
    pub year_month_duration: Option<SimpleTypeKey>,
    /// xs:dayTimeDuration (XSD 1.1)
    pub day_time_duration: Option<SimpleTypeKey>,
    /// xs:dateTimeStamp (XSD 1.1)
    pub datetime_stamp: Option<SimpleTypeKey>,
    /// xs:untypedAtomic (XSD 1.1)
    pub untyped_atomic: Option<SimpleTypeKey>,

    // Binary types
    /// xs:hexBinary
    pub hex_binary: SimpleTypeKey,
    /// xs:base64Binary
    pub base64_binary: SimpleTypeKey,

    // URI types
    /// xs:anyURI
    pub any_uri: SimpleTypeKey,

    // QName types
    /// xs:QName
    pub qname: SimpleTypeKey,
    /// xs:NOTATION
    pub notation: SimpleTypeKey,

    // List types (built-in)
    /// xs:NMTOKENS
    pub nmtokens: SimpleTypeKey,
    /// xs:IDREFS
    pub idrefs: SimpleTypeKey,
    /// xs:ENTITIES
    pub entities: SimpleTypeKey,

    // Lookup maps for fast access
    /// Map from XmlTypeCode to SimpleTypeKey
    by_type_code: HashMap<XmlTypeCode, SimpleTypeKey>,
    /// Map from local name NameId to SimpleTypeKey (for XS namespace)
    by_local_name: HashMap<NameId, SimpleTypeKey>,
}

impl BuiltinTypes {
    /// Initialize all built-in types and register them in the schema set.
    ///
    /// This creates `SimpleTypeDefData` entries in the arenas for all
    /// 47 XSD 1.0 built-in types (plus 3 additional XSD 1.1 types if
    /// the version is XSD 1.1).
    pub fn new(schema_set: &mut SchemaSet) -> Self {
        let xsd_version = schema_set.xsd_version;
        let xs_ns = Some(well_known::XS_NAMESPACE);

        let any_type_name = schema_set.name_table.add("anyType");
        let any_type = schema_set.arenas.alloc_complex_type(ComplexTypeDefData {
            name: Some(any_type_name),
            target_namespace: xs_ns,
            base_type: None,
            derivation_method: None,
            content: ComplexContentResult::Complex(ComplexContentDefResult {
                particle: Some(ParticleResult {
                    term: ParticleTerm::Any(WildcardResult {
                        namespace: WildcardNamespace::Any,
                        process_contents: ProcessContents::Lax,
                        not_namespace: Vec::new(),
                        not_qname: Vec::new(),
                        id: None,
                        annotation: None,
                        source: None,
                    }),
                    min_occurs: 0,
                    max_occurs: None,
                    source: None,
                }),
                derivation: DerivationMethod::Restriction,
                mixed: true,
                base_type: None,
                open_content: None,
                attributes: Vec::new(),
                attribute_groups: Vec::new(),
                attribute_wildcard: Some(WildcardResult {
                    namespace: WildcardNamespace::Any,
                    process_contents: ProcessContents::Lax,
                    not_namespace: Vec::new(),
                    not_qname: Vec::new(),
                    id: None,
                    annotation: None,
                    source: None,
                }),
                assertions: Vec::new(),
                id: None,
                derivation_id: None,
                source: None,
            }),
            open_content: None,
            attributes: Vec::new(),
            attribute_groups: Vec::new(),
            attribute_wildcard: None,
            mixed: true,
            is_abstract: false,
            final_derivation: crate::schema::model::DerivationSet::empty(),
            block: crate::schema::model::DerivationSet::empty(),
            default_attributes_apply: true,
            id: None,
            #[cfg(feature = "xsd11")]
            assertions: Vec::new(),
            #[cfg(feature = "xsd11")]
            xpath_default_namespace: None,
            annotation: None,
            source: None,
            // Resolved references (built-in types have no unresolved references)
            resolved_base_type: None,
            resolved_attribute_groups: Vec::new(),
            resolved_attributes: Vec::new(),
            resolved_content_particle_types: Vec::new(),
            resolved_content_particle_elements: Vec::new(),
        });

        // Helper to create and register a built-in type
        let mut create_type = |builtin: BuiltInType| -> SimpleTypeKey {
            let local_name = builtin.local_name();
            let name_id = schema_set.name_table.add(local_name);
            let variety = match builtin {
                BuiltInType::NMTOKENS | BuiltInType::IDREFS | BuiltInType::ENTITIES => {
                    crate::parser::frames::SimpleTypeVariety::List
                }
                _ => crate::parser::frames::SimpleTypeVariety::Atomic,
            };

            let data = SimpleTypeDefData {
                name: Some(name_id),
                target_namespace: xs_ns,
                variety,
                base_type: None,
                item_type: None,
                member_types: Vec::new(),
                facets: crate::types::facets::FacetSet::new(),
                final_derivation: crate::schema::model::DerivationSet::empty(),
                id: None,
                derivation_id: None,
                annotation: None,
                source: None,
                // Resolved references (built-in types have no unresolved references)
                resolved_base_type: None,
                resolved_item_type: None,
                resolved_member_types: Vec::new(),
            };

            schema_set.arenas.alloc_simple_type(data)
        };

        // Create all XSD 1.0 types
        let any_simple_type = create_type(BuiltInType::AnySimpleType);
        let string = create_type(BuiltInType::String);
        let normalized_string = create_type(BuiltInType::NormalizedString);
        let token = create_type(BuiltInType::Token);
        let language = create_type(BuiltInType::Language);
        let nmtoken = create_type(BuiltInType::NMTOKEN);
        let name = create_type(BuiltInType::Name);
        let ncname = create_type(BuiltInType::NCName);
        let id = create_type(BuiltInType::ID);
        let idref = create_type(BuiltInType::IDREF);
        let entity = create_type(BuiltInType::ENTITY);

        let boolean = create_type(BuiltInType::Boolean);
        let decimal = create_type(BuiltInType::Decimal);
        let float = create_type(BuiltInType::Float);
        let double = create_type(BuiltInType::Double);
        let integer = create_type(BuiltInType::Integer);
        let non_positive_integer = create_type(BuiltInType::NonPositiveInteger);
        let negative_integer = create_type(BuiltInType::NegativeInteger);
        let long = create_type(BuiltInType::Long);
        let int = create_type(BuiltInType::Int);
        let short = create_type(BuiltInType::Short);
        let byte = create_type(BuiltInType::Byte);
        let non_negative_integer = create_type(BuiltInType::NonNegativeInteger);
        let unsigned_long = create_type(BuiltInType::UnsignedLong);
        let unsigned_int = create_type(BuiltInType::UnsignedInt);
        let unsigned_short = create_type(BuiltInType::UnsignedShort);
        let unsigned_byte = create_type(BuiltInType::UnsignedByte);
        let positive_integer = create_type(BuiltInType::PositiveInteger);

        let duration = create_type(BuiltInType::Duration);
        let datetime = create_type(BuiltInType::DateTime);
        let time = create_type(BuiltInType::Time);
        let date = create_type(BuiltInType::Date);
        let g_year_month = create_type(BuiltInType::GYearMonth);
        let g_year = create_type(BuiltInType::GYear);
        let g_month_day = create_type(BuiltInType::GMonthDay);
        let g_day = create_type(BuiltInType::GDay);
        let g_month = create_type(BuiltInType::GMonth);

        let hex_binary = create_type(BuiltInType::HexBinary);
        let base64_binary = create_type(BuiltInType::Base64Binary);

        let any_uri = create_type(BuiltInType::AnyURI);
        let qname = create_type(BuiltInType::QName);
        let notation = create_type(BuiltInType::NOTATION);

        let nmtokens = create_type(BuiltInType::NMTOKENS);
        let idrefs = create_type(BuiltInType::IDREFS);
        let entities = create_type(BuiltInType::ENTITIES);

        // XSD 1.1 types (only if XSD 1.1 mode)
        let (any_atomic_type, untyped_atomic, year_month_duration, day_time_duration, datetime_stamp) =
            if xsd_version == XsdVersion::V1_1 {
                (
                    Some(create_type(BuiltInType::AnyAtomicType)),
                    Some(create_type(BuiltInType::UntypedAtomic)),
                    Some(create_type(BuiltInType::YearMonthDuration)),
                    Some(create_type(BuiltInType::DayTimeDuration)),
                    Some(create_type(BuiltInType::DateTimeStamp)),
                )
            } else {
                (None, None, None, None, None)
            };

        // Build lookup maps
        let mut by_type_code = HashMap::new();
        let mut by_local_name = HashMap::new();

        // Helper to add to lookup maps
        let mut add_to_maps = |builtin: BuiltInType, key: SimpleTypeKey| {
            let type_code = builtin.type_code();
            by_type_code.insert(type_code, key);

            let local_name = builtin.local_name();
            let name_id = schema_set.name_table.add(local_name);
            by_local_name.insert(name_id, key);
        };

        // Add all types to maps
        add_to_maps(BuiltInType::AnySimpleType, any_simple_type);
        add_to_maps(BuiltInType::String, string);
        add_to_maps(BuiltInType::NormalizedString, normalized_string);
        add_to_maps(BuiltInType::Token, token);
        add_to_maps(BuiltInType::Language, language);
        add_to_maps(BuiltInType::NMTOKEN, nmtoken);
        add_to_maps(BuiltInType::Name, name);
        add_to_maps(BuiltInType::NCName, ncname);
        add_to_maps(BuiltInType::ID, id);
        add_to_maps(BuiltInType::IDREF, idref);
        add_to_maps(BuiltInType::ENTITY, entity);
        add_to_maps(BuiltInType::Boolean, boolean);
        add_to_maps(BuiltInType::Decimal, decimal);
        add_to_maps(BuiltInType::Float, float);
        add_to_maps(BuiltInType::Double, double);
        add_to_maps(BuiltInType::Integer, integer);
        add_to_maps(BuiltInType::NonPositiveInteger, non_positive_integer);
        add_to_maps(BuiltInType::NegativeInteger, negative_integer);
        add_to_maps(BuiltInType::Long, long);
        add_to_maps(BuiltInType::Int, int);
        add_to_maps(BuiltInType::Short, short);
        add_to_maps(BuiltInType::Byte, byte);
        add_to_maps(BuiltInType::NonNegativeInteger, non_negative_integer);
        add_to_maps(BuiltInType::UnsignedLong, unsigned_long);
        add_to_maps(BuiltInType::UnsignedInt, unsigned_int);
        add_to_maps(BuiltInType::UnsignedShort, unsigned_short);
        add_to_maps(BuiltInType::UnsignedByte, unsigned_byte);
        add_to_maps(BuiltInType::PositiveInteger, positive_integer);
        add_to_maps(BuiltInType::Duration, duration);
        add_to_maps(BuiltInType::DateTime, datetime);
        add_to_maps(BuiltInType::Time, time);
        add_to_maps(BuiltInType::Date, date);
        add_to_maps(BuiltInType::GYearMonth, g_year_month);
        add_to_maps(BuiltInType::GYear, g_year);
        add_to_maps(BuiltInType::GMonthDay, g_month_day);
        add_to_maps(BuiltInType::GDay, g_day);
        add_to_maps(BuiltInType::GMonth, g_month);
        add_to_maps(BuiltInType::HexBinary, hex_binary);
        add_to_maps(BuiltInType::Base64Binary, base64_binary);
        add_to_maps(BuiltInType::AnyURI, any_uri);
        add_to_maps(BuiltInType::QName, qname);
        add_to_maps(BuiltInType::NOTATION, notation);
        add_to_maps(BuiltInType::NMTOKENS, nmtokens);
        add_to_maps(BuiltInType::IDREFS, idrefs);
        add_to_maps(BuiltInType::ENTITIES, entities);

        // Add XSD 1.1 types to maps
        if let Some(key) = any_atomic_type {
            add_to_maps(BuiltInType::AnyAtomicType, key);
        }
        if let Some(key) = untyped_atomic {
            add_to_maps(BuiltInType::UntypedAtomic, key);
        }
        if let Some(key) = year_month_duration {
            add_to_maps(BuiltInType::YearMonthDuration, key);
        }
        if let Some(key) = day_time_duration {
            add_to_maps(BuiltInType::DayTimeDuration, key);
        }
        if let Some(key) = datetime_stamp {
            add_to_maps(BuiltInType::DateTimeStamp, key);
        }

        // Resolve item types for built-in list types so that
        // validate_list_type can validate each item individually.
        for (list_key, item_key) in [(nmtokens, nmtoken), (idrefs, idref), (entities, entity)] {
            if let Some(st) = schema_set.arenas.get_simple_type_mut(list_key) {
                st.resolved_item_type = Some(TypeKey::Simple(item_key));
            }
        }

        Self {
            any_type,
            any_simple_type,
            any_atomic_type,
            string,
            normalized_string,
            token,
            language,
            nmtoken,
            name,
            ncname,
            id,
            idref,
            entity,
            boolean,
            decimal,
            float,
            double,
            integer,
            non_positive_integer,
            negative_integer,
            long,
            int,
            short,
            byte,
            non_negative_integer,
            unsigned_long,
            unsigned_int,
            unsigned_short,
            unsigned_byte,
            positive_integer,
            duration,
            datetime,
            time,
            date,
            g_year_month,
            g_year,
            g_month_day,
            g_day,
            g_month,
            year_month_duration,
            day_time_duration,
            datetime_stamp,
            untyped_atomic,
            hex_binary,
            base64_binary,
            any_uri,
            qname,
            notation,
            nmtokens,
            idrefs,
            entities,
            by_type_code,
            by_local_name,
        }
    }

    /// Get a built-in type by its XmlTypeCode.
    ///
    /// Returns `None` for node types, AnyType, and other non-simple type codes.
    pub fn get_by_type_code(&self, code: XmlTypeCode) -> Option<SimpleTypeKey> {
        self.by_type_code.get(&code).copied()
    }

    /// Get a built-in type by its local name (within the XS namespace).
    ///
    /// The local name must be interned in the NameTable (passed as NameId).
    pub fn get_by_local_name(&self, name: NameId) -> Option<SimpleTypeKey> {
        self.by_local_name.get(&name).copied()
    }

    /// Get the XmlTypeCode for a built-in type key.
    ///
    /// Returns `None` if the key is not a built-in type.
    pub fn get_type_code(&self, key: SimpleTypeKey) -> Option<XmlTypeCode> {
        // Iterate over the map to find the type code for this key
        for (&code, &k) in &self.by_type_code {
            if k == key {
                return Some(code);
            }
        }
        None
    }

    /// Check if a type key is a built-in type.
    pub fn is_builtin(&self, key: SimpleTypeKey) -> bool {
        self.get_type_code(key).is_some()
    }

    /// Get the base type for a built-in type (for derivation hierarchy).
    ///
    /// Returns the immediate base type in the XSD type hierarchy.
    /// Returns `None` for `anySimpleType` (the root of simple types).
    pub fn get_base_type(&self, key: SimpleTypeKey) -> Option<SimpleTypeKey> {
        let code = self.get_type_code(key)?;
        let base_code = get_builtin_base_type(code)?;
        self.get_by_type_code(base_code)
    }

    /// Check if `derived` derives from `base` (transitively).
    ///
    /// Returns `true` if:
    /// - `derived == base`, or
    /// - `derived` has `base` somewhere in its derivation chain
    pub fn derives_from(&self, derived: SimpleTypeKey, base: SimpleTypeKey) -> bool {
        if derived == base {
            return true;
        }

        // Walk up the derivation chain
        let mut current = derived;
        while let Some(parent) = self.get_base_type(current) {
            if parent == base {
                return true;
            }
            current = parent;
        }

        false
    }

    /// Returns the number of registered built-in types.
    ///
    /// This is 47 for XSD 1.0, or 50+ for XSD 1.1.
    pub fn count(&self) -> usize {
        self.by_type_code.len()
    }
}

/// Get the base type code for a built-in type.
///
/// Returns the immediate parent in the XSD derivation hierarchy.
fn get_builtin_base_type(code: XmlTypeCode) -> Option<XmlTypeCode> {
    match code {
        // anySimpleType has no base (it's the root of simple types)
        XmlTypeCode::AnySimpleType => None,

        // anyAtomicType derives from anySimpleType
        XmlTypeCode::AnyAtomicType => Some(XmlTypeCode::AnySimpleType),

        // All primitives derive from anyAtomicType (conceptually)
        // But for XSD 1.0, they derive from anySimpleType
        XmlTypeCode::String
        | XmlTypeCode::Boolean
        | XmlTypeCode::Decimal
        | XmlTypeCode::Float
        | XmlTypeCode::Double
        | XmlTypeCode::Duration
        | XmlTypeCode::DateTime
        | XmlTypeCode::Time
        | XmlTypeCode::Date
        | XmlTypeCode::GYearMonth
        | XmlTypeCode::GYear
        | XmlTypeCode::GMonthDay
        | XmlTypeCode::GDay
        | XmlTypeCode::GMonth
        | XmlTypeCode::HexBinary
        | XmlTypeCode::Base64Binary
        | XmlTypeCode::AnyUri
        | XmlTypeCode::QName
        | XmlTypeCode::Notation => Some(XmlTypeCode::AnySimpleType),

        // String-derived types
        XmlTypeCode::NormalizedString => Some(XmlTypeCode::String),
        XmlTypeCode::Token => Some(XmlTypeCode::NormalizedString),
        XmlTypeCode::Language => Some(XmlTypeCode::Token),
        XmlTypeCode::NmToken => Some(XmlTypeCode::Token),
        XmlTypeCode::Name => Some(XmlTypeCode::Token),
        XmlTypeCode::NCName => Some(XmlTypeCode::Name),
        XmlTypeCode::Id => Some(XmlTypeCode::NCName),
        XmlTypeCode::IdRef => Some(XmlTypeCode::NCName),
        XmlTypeCode::Entity => Some(XmlTypeCode::NCName),

        // Decimal-derived types (integer hierarchy)
        XmlTypeCode::Integer => Some(XmlTypeCode::Decimal),
        XmlTypeCode::NonPositiveInteger => Some(XmlTypeCode::Integer),
        XmlTypeCode::NegativeInteger => Some(XmlTypeCode::NonPositiveInteger),
        XmlTypeCode::Long => Some(XmlTypeCode::Integer),
        XmlTypeCode::Int => Some(XmlTypeCode::Long),
        XmlTypeCode::Short => Some(XmlTypeCode::Int),
        XmlTypeCode::Byte => Some(XmlTypeCode::Short),
        XmlTypeCode::NonNegativeInteger => Some(XmlTypeCode::Integer),
        XmlTypeCode::UnsignedLong => Some(XmlTypeCode::NonNegativeInteger),
        XmlTypeCode::UnsignedInt => Some(XmlTypeCode::UnsignedLong),
        XmlTypeCode::UnsignedShort => Some(XmlTypeCode::UnsignedInt),
        XmlTypeCode::UnsignedByte => Some(XmlTypeCode::UnsignedShort),
        XmlTypeCode::PositiveInteger => Some(XmlTypeCode::NonNegativeInteger),

        // XSD 1.1 types
        XmlTypeCode::UntypedAtomic => Some(XmlTypeCode::AnyAtomicType),
        XmlTypeCode::YearMonthDuration => Some(XmlTypeCode::Duration),
        XmlTypeCode::DayTimeDuration => Some(XmlTypeCode::Duration),
        XmlTypeCode::DateTimeStamp => Some(XmlTypeCode::DateTime),

        // List types derive from anySimpleType
        XmlTypeCode::NmTokens | XmlTypeCode::IdRefs | XmlTypeCode::Entities => {
            Some(XmlTypeCode::AnySimpleType)
        }

        // Non-simple types have no base in this hierarchy
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_schema_set() -> SchemaSet {
        SchemaSet::new()
    }

    fn create_test_schema_set_v11() -> SchemaSet {
        SchemaSet::with_version(XsdVersion::V1_1)
    }

    #[test]
    fn test_builtin_types_creation() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // Check that we have the expected number of types for XSD 1.0
        // Total 50 built-in types - 5 XSD 1.1 types = 45
        assert_eq!(builtin.count(), 45);
    }

    #[test]
    fn test_builtin_types_xsd11() {
        let mut schema_set = create_test_schema_set_v11();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // Check that we have the expected number of types for XSD 1.1
        // All 50 built-in types are registered
        assert_eq!(builtin.count(), 50);

        // XSD 1.1 types should be present
        assert!(builtin.any_atomic_type.is_some());
        assert!(builtin.untyped_atomic.is_some());
        assert!(builtin.year_month_duration.is_some());
        assert!(builtin.day_time_duration.is_some());
        assert!(builtin.datetime_stamp.is_some());
    }

    #[test]
    fn test_builtin_types_xsd10_no_xsd11() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // XSD 1.1 types should not be present in XSD 1.0 mode
        assert!(builtin.any_atomic_type.is_none());
        assert!(builtin.untyped_atomic.is_none());
        assert!(builtin.year_month_duration.is_none());
        assert!(builtin.day_time_duration.is_none());
        assert!(builtin.datetime_stamp.is_none());
    }

    #[test]
    fn test_get_by_type_code() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        assert_eq!(
            builtin.get_by_type_code(XmlTypeCode::String),
            Some(builtin.string)
        );
        assert_eq!(
            builtin.get_by_type_code(XmlTypeCode::Integer),
            Some(builtin.integer)
        );
        assert_eq!(
            builtin.get_by_type_code(XmlTypeCode::DateTime),
            Some(builtin.datetime)
        );
        assert_eq!(
            builtin.get_by_type_code(XmlTypeCode::AnySimpleType),
            Some(builtin.any_simple_type)
        );

        // Node types should not be found
        assert!(builtin.get_by_type_code(XmlTypeCode::Element).is_none());
        assert!(builtin.get_by_type_code(XmlTypeCode::AnyType).is_none());
    }

    #[test]
    fn test_get_by_local_name() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        let string_id = schema_set.name_table.add("string");
        assert_eq!(builtin.get_by_local_name(string_id), Some(builtin.string));

        let integer_id = schema_set.name_table.add("integer");
        assert_eq!(builtin.get_by_local_name(integer_id), Some(builtin.integer));

        let nonexistent_id = schema_set.name_table.add("nonExistent");
        assert!(builtin.get_by_local_name(nonexistent_id).is_none());
    }

    #[test]
    fn test_get_type_code() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        assert_eq!(
            builtin.get_type_code(builtin.string),
            Some(XmlTypeCode::String)
        );
        assert_eq!(
            builtin.get_type_code(builtin.integer),
            Some(XmlTypeCode::Integer)
        );
        assert_eq!(
            builtin.get_type_code(builtin.any_simple_type),
            Some(XmlTypeCode::AnySimpleType)
        );
    }

    #[test]
    fn test_is_builtin() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        assert!(builtin.is_builtin(builtin.string));
        assert!(builtin.is_builtin(builtin.integer));
        assert!(builtin.is_builtin(builtin.any_simple_type));

        // Create a non-builtin type
        let custom_type = schema_set.arenas.alloc_simple_type(SimpleTypeDefData {
            name: Some(NameId(999)),
            target_namespace: None,
            variety: crate::parser::frames::SimpleTypeVariety::Atomic,
            base_type: None,
            item_type: None,
            member_types: Vec::new(),
            facets: crate::types::facets::FacetSet::new(),
            final_derivation: crate::schema::model::DerivationSet::empty(),
            id: None,
            derivation_id: None,
            annotation: None,
            source: None,
            // Resolved references
            resolved_base_type: None,
            resolved_item_type: None,
            resolved_member_types: Vec::new(),
        });
        assert!(!builtin.is_builtin(custom_type));
    }

    #[test]
    fn test_derives_from_same() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // A type derives from itself
        assert!(builtin.derives_from(builtin.string, builtin.string));
        assert!(builtin.derives_from(builtin.integer, builtin.integer));
    }

    #[test]
    fn test_derives_from_direct() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // Direct derivation
        assert!(builtin.derives_from(builtin.normalized_string, builtin.string));
        assert!(builtin.derives_from(builtin.integer, builtin.decimal));
        assert!(builtin.derives_from(builtin.long, builtin.integer));
    }

    #[test]
    fn test_derives_from_transitive() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // Transitive derivation: byte < short < int < long < integer < decimal
        assert!(builtin.derives_from(builtin.byte, builtin.decimal));
        assert!(builtin.derives_from(builtin.byte, builtin.integer));
        assert!(builtin.derives_from(builtin.byte, builtin.long));
        assert!(builtin.derives_from(builtin.int, builtin.decimal));

        // NCName < Name < token < normalizedString < string
        assert!(builtin.derives_from(builtin.ncname, builtin.string));
        assert!(builtin.derives_from(builtin.id, builtin.string));
    }

    #[test]
    fn test_derives_from_negative() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // Not derived
        assert!(!builtin.derives_from(builtin.string, builtin.integer));
        assert!(!builtin.derives_from(builtin.decimal, builtin.integer)); // Reverse
        assert!(!builtin.derives_from(builtin.float, builtin.double));
    }

    #[test]
    fn test_derives_from_any_simple_type() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // All simple types derive from anySimpleType
        assert!(builtin.derives_from(builtin.string, builtin.any_simple_type));
        assert!(builtin.derives_from(builtin.integer, builtin.any_simple_type));
        assert!(builtin.derives_from(builtin.byte, builtin.any_simple_type));
        assert!(builtin.derives_from(builtin.nmtokens, builtin.any_simple_type));
    }

    #[test]
    fn test_get_base_type() {
        let mut schema_set = create_test_schema_set();
        let builtin = BuiltinTypes::new(&mut schema_set);

        // anySimpleType has no base
        assert!(builtin.get_base_type(builtin.any_simple_type).is_none());

        // Primitives derive from anySimpleType
        assert_eq!(
            builtin.get_base_type(builtin.string),
            Some(builtin.any_simple_type)
        );

        // normalizedString derives from string
        assert_eq!(
            builtin.get_base_type(builtin.normalized_string),
            Some(builtin.string)
        );

        // integer derives from decimal
        assert_eq!(
            builtin.get_base_type(builtin.integer),
            Some(builtin.decimal)
        );
    }

    #[test]
    fn test_builtin_base_type_hierarchy() {
        // Test the get_builtin_base_type function directly
        assert_eq!(get_builtin_base_type(XmlTypeCode::AnySimpleType), None);
        assert_eq!(
            get_builtin_base_type(XmlTypeCode::String),
            Some(XmlTypeCode::AnySimpleType)
        );
        assert_eq!(
            get_builtin_base_type(XmlTypeCode::NormalizedString),
            Some(XmlTypeCode::String)
        );
        assert_eq!(
            get_builtin_base_type(XmlTypeCode::Token),
            Some(XmlTypeCode::NormalizedString)
        );
        assert_eq!(
            get_builtin_base_type(XmlTypeCode::NCName),
            Some(XmlTypeCode::Name)
        );
        assert_eq!(
            get_builtin_base_type(XmlTypeCode::Integer),
            Some(XmlTypeCode::Decimal)
        );
        assert_eq!(
            get_builtin_base_type(XmlTypeCode::Long),
            Some(XmlTypeCode::Integer)
        );
        assert_eq!(
            get_builtin_base_type(XmlTypeCode::Byte),
            Some(XmlTypeCode::Short)
        );
    }
}
