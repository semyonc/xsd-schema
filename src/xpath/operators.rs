//! XPath operator evaluation helpers.
//!
//! This module hosts AST-only operator evaluation logic, using `XmlValue`
//! and `XmlTypeCode` directly (no ValueProxy layer). See
//! `XPATH_OPERATORS_DESIGN.md` for the target behavior.

use std::cmp::Ordering;

use chrono::Local;
use num_bigint::BigInt;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;

use crate::xpath::cast::cast_to;
use crate::xpath::context::XPathContext;
use crate::xpath::error::XPathError;
use crate::xpath::iterator::{BufferedNodeIterator, XmlItemRef, XmlNodeIterator};
use crate::xpath::type_info::type_code_to_name;
use crate::xpath::DomNavigator;
use crate::types::value::{
    DateTimeValue, DateValue, DayTimeDurationValue, DurationValue, TimeValue,
    TimezoneOffset, YearMonthDurationValue, XmlAtomicValue, XmlValue, XmlValueKind,
};
use crate::types::XmlTypeCode;
use crate::xpath::ast::{BinaryOpKind, UnaryOpKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumericClass {
    Byte,
    UnsignedByte,
    Short,
    UnsignedShort,
    Int,
    UnsignedInt,
    Long,
    UnsignedLong,
    Integer,
    Decimal,
    Float,
    Double,
}

#[derive(Debug, Clone)]
enum NumericValue {
    Integer(BigInt),
    Decimal(Decimal),
    Float(f32),
    Double(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Promote {
    Left,
    Right,
    None,
}

/// Evaluate a unary operator for a single atomic value.
pub fn eval_unary(op: UnaryOpKind, value: &XmlValue) -> Result<XmlValue, XPathError> {
    match op {
        UnaryOpKind::Identity => Ok(value.clone()),
        UnaryOpKind::Negate => eval_numeric_unary(value),
        UnaryOpKind::BooleanNot => {
            // TODO: Implement boolean not when needed
            Err(XPathError::internal("BooleanNot operator not yet implemented"))
        }
        UnaryOpKind::Atomize => {
            // TODO: Implement atomization when needed
            Err(XPathError::internal("Atomize operator not yet implemented"))
        }
    }
}

/// Evaluate a binary operator for two atomic values.
pub fn eval_binary(
    op: BinaryOpKind,
    left: &XmlValue,
    right: &XmlValue,
) -> Result<XmlValue, XPathError> {
    match op {
        BinaryOpKind::Add | BinaryOpKind::Sub | BinaryOpKind::Mul | BinaryOpKind::Div => {
            if let Some(result) = eval_temporal_binary(op, left, right)? {
                return Ok(result);
            }
            eval_numeric_binary(op, left, right)
        }
        BinaryOpKind::IDiv | BinaryOpKind::Mod => eval_numeric_binary(op, left, right),
        BinaryOpKind::GeneralEq | BinaryOpKind::ValueEq => {
            Ok(XmlValue::boolean(compare_eq(left, right)?))
        }
        BinaryOpKind::GeneralNe | BinaryOpKind::ValueNe => {
            Ok(XmlValue::boolean(!compare_eq(left, right)?))
        }
        BinaryOpKind::GeneralGt | BinaryOpKind::ValueGt => {
            Ok(XmlValue::boolean(compare_gt(left, right)?))
        }
        BinaryOpKind::GeneralGe | BinaryOpKind::ValueGe => {
            Ok(XmlValue::boolean(compare_ge(left, right)?))
        }
        BinaryOpKind::GeneralLt | BinaryOpKind::ValueLt => {
            Ok(XmlValue::boolean(compare_lt(left, right)?))
        }
        BinaryOpKind::GeneralLe | BinaryOpKind::ValueLe => {
            Ok(XmlValue::boolean(compare_le(left, right)?))
        }
        BinaryOpKind::And | BinaryOpKind::Or => eval_boolean_logic(op, left, right),
        BinaryOpKind::Is | BinaryOpKind::Before | BinaryOpKind::After => {
            Err(unsupported_operator(op, left, right))
        }
        BinaryOpKind::Union | BinaryOpKind::Intersect | BinaryOpKind::Except => {
            Err(unsupported_operator(op, left, right))
        }
    }
}

/// Evaluate an XPath range expression (`expr to expr`).
pub fn eval_range(start: &XmlValue, end: &XmlValue) -> Result<Vec<XmlValue>, XPathError> {
    let start_class = numeric_class(start.type_code)
        .ok_or_else(|| XPathError::internal("Range start must be numeric"))?;
    let end_class = numeric_class(end.type_code)
        .ok_or_else(|| XPathError::internal("Range end must be numeric"))?;

    if !is_integer_class(start_class) || !is_integer_class(end_class) {
        return Err(XPathError::internal("Range bounds must be integer types"));
    }

    let start_val = to_integer_value(start)?;
    let end_val = to_integer_value(end)?;

    if start_val > end_val {
        return Ok(Vec::new());
    }

    let mut result = Vec::new();
    let mut current = start_val;
    let one = BigInt::from(1);
    while current <= end_val {
        result.push(XmlValue {
            type_code: XmlTypeCode::Integer,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Integer(current.clone())),
        });
        current += &one;
    }

    Ok(result)
}

/// Check if a type code is numeric for operator dispatch.
pub fn is_numeric(code: XmlTypeCode) -> bool {
    code.is_numeric()
}

fn eval_boolean_logic(
    op: BinaryOpKind,
    left: &XmlValue,
    right: &XmlValue,
) -> Result<XmlValue, XPathError> {
    let left_bool = left.as_boolean().ok_or_else(|| {
        XPathError::internal("Boolean operator requires boolean operands")
    })?;
    let right_bool = right.as_boolean().ok_or_else(|| {
        XPathError::internal("Boolean operator requires boolean operands")
    })?;

    let result = match op {
        BinaryOpKind::And => left_bool && right_bool,
        BinaryOpKind::Or => left_bool || right_bool,
        _ => return Err(XPathError::internal("Unexpected boolean operator")),
    };

    Ok(XmlValue::boolean(result))
}

fn compare_eq(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let left = unwrap_union_value(left);
    let right = unwrap_union_value(right);

    if let Some(result) = compare_temporal_eq(left, right)? {
        return Ok(result);
    }

    if left.type_code.is_list() || right.type_code.is_list() {
        if left.type_code != right.type_code {
            return Err(operator_not_defined("op:eq", left, right));
        }
        return Ok(list_values_equal(left, right));
    }

    if left.type_code.is_numeric() && right.type_code.is_numeric() {
        return numeric_eq(left, right);
    }

    if left.type_code == XmlTypeCode::Boolean && right.type_code == XmlTypeCode::Boolean {
        return Ok(left.as_boolean() == right.as_boolean());
    }

    if is_string_like(left.type_code) && is_string_like(right.type_code) {
        return Ok(left.to_string_value() == right.to_string_value());
    }

    if left.type_code == right.type_code {
        return Ok(left.value == right.value);
    }

    Err(operator_not_defined("op:eq", left, right))
}

fn compare_gt(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let left = unwrap_union_value(left);
    let right = unwrap_union_value(right);

    if let Some(result) = compare_temporal_gt(left, right)? {
        return Ok(result);
    }

    if left.type_code.is_numeric() && right.type_code.is_numeric() {
        return numeric_gt(left, right);
    }

    if is_string_like(left.type_code) && is_string_like(right.type_code) {
        let left_value = left.to_string_value();
        let right_value = right.to_string_value();
        return Ok(compare_string_values(&left_value, &right_value) == Ordering::Greater);
    }

    Err(operator_not_defined("op:gt", left, right))
}

fn compare_ge(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    match compare_eq(left, right) {
        Ok(true) => Ok(true),
        Ok(false) => compare_gt(left, right),
        Err(err) if is_operator_not_defined(&err) => compare_gt(left, right),
        Err(err) => Err(err),
    }
}

fn compare_lt(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    compare_gt(right, left)
}

fn compare_le(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    match compare_eq(left, right) {
        Ok(true) => Ok(true),
        Ok(false) => compare_lt(left, right),
        Err(err) if is_operator_not_defined(&err) => compare_lt(left, right),
        Err(err) => Err(err),
    }
}

fn eval_temporal_binary(
    op: BinaryOpKind,
    left: &XmlValue,
    right: &XmlValue,
) -> Result<Option<XmlValue>, XPathError> {
    if !is_temporal_type(left.type_code) && !is_temporal_type(right.type_code) {
        return Ok(None);
    }

    let result = match op {
        BinaryOpKind::Add => eval_temporal_add(left, right),
        BinaryOpKind::Sub => eval_temporal_sub(left, right),
        BinaryOpKind::Mul => eval_temporal_mul(left, right),
        BinaryOpKind::Div => eval_temporal_div(left, right),
        _ => return Ok(None),
    }?;

    Ok(Some(result))
}

fn compare_temporal_eq(
    left: &XmlValue,
    right: &XmlValue,
) -> Result<Option<bool>, XPathError> {
    if !is_temporal_type(left.type_code) && !is_temporal_type(right.type_code) {
        return Ok(None);
    }

    if is_date_time_code(left.type_code) || is_date_time_code(right.type_code) {
        if !(is_date_time_code(left.type_code) && is_date_time_code(right.type_code)) {
            return Err(operator_not_defined("op:eq", left, right));
        }
        let left_dt = as_datetime(left)
            .ok_or_else(|| XPathError::internal("Expected dateTime value"))?;
        let right_dt = as_datetime(right)
            .ok_or_else(|| XPathError::internal("Expected dateTime value"))?;
        return Ok(Some(compare_datetime_eq(left_dt, right_dt)?));
    }

    if left.type_code == XmlTypeCode::Date || right.type_code == XmlTypeCode::Date {
        if left.type_code != XmlTypeCode::Date || right.type_code != XmlTypeCode::Date {
            return Err(operator_not_defined("op:eq", left, right));
        }
        let left_date = as_date(left)
            .ok_or_else(|| XPathError::internal("Expected date value"))?;
        let right_date = as_date(right)
            .ok_or_else(|| XPathError::internal("Expected date value"))?;
        return Ok(Some(compare_date_eq(left_date, right_date)?));
    }

    if left.type_code == XmlTypeCode::Time || right.type_code == XmlTypeCode::Time {
        if left.type_code != XmlTypeCode::Time || right.type_code != XmlTypeCode::Time {
            return Err(operator_not_defined("op:eq", left, right));
        }
        let left_time = as_time(left)
            .ok_or_else(|| XPathError::internal("Expected time value"))?;
        let right_time = as_time(right)
            .ok_or_else(|| XPathError::internal("Expected time value"))?;
        return Ok(Some(compare_time_eq(left_time, right_time)?));
    }

    if is_duration_code(left.type_code) || is_duration_code(right.type_code) {
        if !(is_duration_code(left.type_code) && is_duration_code(right.type_code)) {
            return Err(operator_not_defined("op:eq", left, right));
        }
        let left_parts = duration_parts(left)?
            .ok_or_else(|| XPathError::internal("Expected duration value"))?;
        let right_parts = duration_parts(right)?
            .ok_or_else(|| XPathError::internal("Expected duration value"))?;
        return Ok(Some(left_parts == right_parts));
    }

    Err(operator_not_defined("op:eq", left, right))
}

fn compare_temporal_gt(
    left: &XmlValue,
    right: &XmlValue,
) -> Result<Option<bool>, XPathError> {
    if !is_temporal_type(left.type_code) && !is_temporal_type(right.type_code) {
        return Ok(None);
    }

    if is_date_time_code(left.type_code) || is_date_time_code(right.type_code) {
        if !(is_date_time_code(left.type_code) && is_date_time_code(right.type_code)) {
            return Err(operator_not_defined("op:gt", left, right));
        }
        let left_dt = as_datetime(left)
            .ok_or_else(|| XPathError::internal("Expected dateTime value"))?;
        let right_dt = as_datetime(right)
            .ok_or_else(|| XPathError::internal("Expected dateTime value"))?;
        return Ok(Some(compare_datetime_gt(left_dt, right_dt)?));
    }

    if left.type_code == XmlTypeCode::Date || right.type_code == XmlTypeCode::Date {
        if left.type_code != XmlTypeCode::Date || right.type_code != XmlTypeCode::Date {
            return Err(operator_not_defined("op:gt", left, right));
        }
        let left_date = as_date(left)
            .ok_or_else(|| XPathError::internal("Expected date value"))?;
        let right_date = as_date(right)
            .ok_or_else(|| XPathError::internal("Expected date value"))?;
        return Ok(Some(compare_date_gt(left_date, right_date)?));
    }

    if left.type_code == XmlTypeCode::Time || right.type_code == XmlTypeCode::Time {
        if left.type_code != XmlTypeCode::Time || right.type_code != XmlTypeCode::Time {
            return Err(operator_not_defined("op:gt", left, right));
        }
        let left_time = as_time(left)
            .ok_or_else(|| XPathError::internal("Expected time value"))?;
        let right_time = as_time(right)
            .ok_or_else(|| XPathError::internal("Expected time value"))?;
        return Ok(Some(compare_time_gt(left_time, right_time)?));
    }

    if left.type_code == XmlTypeCode::YearMonthDuration
        || right.type_code == XmlTypeCode::YearMonthDuration
    {
        if left.type_code != XmlTypeCode::YearMonthDuration
            || right.type_code != XmlTypeCode::YearMonthDuration
        {
            return Err(operator_not_defined("op:gt", left, right));
        }
        let left_duration = as_year_month_duration(left)
            .ok_or_else(|| XPathError::internal("Expected yearMonthDuration value"))?;
        let right_duration = as_year_month_duration(right)
            .ok_or_else(|| XPathError::internal("Expected yearMonthDuration value"))?;
        return Ok(Some(
            year_month_total_months(left_duration) > year_month_total_months(right_duration),
        ));
    }

    if left.type_code == XmlTypeCode::DayTimeDuration
        || right.type_code == XmlTypeCode::DayTimeDuration
    {
        if left.type_code != XmlTypeCode::DayTimeDuration
            || right.type_code != XmlTypeCode::DayTimeDuration
        {
            return Err(operator_not_defined("op:gt", left, right));
        }
        let left_duration = as_day_time_duration(left)
            .ok_or_else(|| XPathError::internal("Expected dayTimeDuration value"))?;
        let right_duration = as_day_time_duration(right)
            .ok_or_else(|| XPathError::internal("Expected dayTimeDuration value"))?;
        return Ok(Some(
            day_time_total_seconds(left_duration)?
                > day_time_total_seconds(right_duration)?,
        ));
    }

    Err(operator_not_defined("op:gt", left, right))
}

fn operator_not_defined(op: &str, left: &XmlValue, right: &XmlValue) -> XPathError {
    XPathError::BinaryOperatorNotDefined {
        operator: op.to_string(),
        left_type: type_code_to_name(left.type_code).to_string(),
        right_type: type_code_to_name(right.type_code).to_string(),
    }
}

fn is_operator_not_defined(err: &XPathError) -> bool {
    matches!(err, XPathError::BinaryOperatorNotDefined { .. })
}

fn unwrap_union_value(value: &XmlValue) -> &XmlValue {
    let mut current = value;
    loop {
        match &current.value {
            XmlValueKind::Union(inner) => current = inner,
            _ => return current,
        }
    }
}

fn list_values_equal(left: &XmlValue, right: &XmlValue) -> bool {
    match (&left.value, &right.value) {
        (
            XmlValueKind::List {
                item_type: left_item_type,
                items: left_items,
            },
            XmlValueKind::List {
                item_type: right_item_type,
                items: right_items,
            },
        ) => left_item_type == right_item_type && left_items == right_items,
        _ => false,
    }
}

fn compare_string_values(left: &str, right: &str) -> Ordering {
    left.cmp(right)
}

fn eval_temporal_add(left: &XmlValue, right: &XmlValue) -> Result<XmlValue, XPathError> {
    if let Some(left_dt) = as_datetime(left) {
        if let Some(duration) = as_year_month_duration(right) {
            let result = add_datetime_year_month(left_dt, duration)?;
            return Ok(xml_datetime_value(left.type_code, result));
        }
        if let Some(duration) = as_day_time_duration(right) {
            let result = add_datetime_day_time(left_dt, duration)?;
            return Ok(xml_datetime_value(left.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Add, left, right));
    }

    if let Some(left_date) = as_date(left) {
        if let Some(duration) = as_year_month_duration(right) {
            let result = add_date_year_month(left_date, duration)?;
            return Ok(xml_date_value(left.type_code, result));
        }
        if let Some(duration) = as_day_time_duration(right) {
            let result = add_date_day_time(left_date, duration)?;
            return Ok(xml_date_value(left.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Add, left, right));
    }

    if let Some(left_time) = as_time(left) {
        if let Some(duration) = as_day_time_duration(right) {
            let result = add_time_day_time(left_time, duration)?;
            return Ok(xml_time_value(left.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Add, left, right));
    }

    if let Some(left_duration) = as_year_month_duration(left) {
        if let Some(right_duration) = as_year_month_duration(right) {
            let total = year_month_total_months(left_duration)
                + year_month_total_months(right_duration);
            return Ok(xml_year_month_duration_value(
                year_month_from_months(total)?,
            ));
        }
        if let Some(right_dt) = as_datetime(right) {
            let result = add_datetime_year_month(right_dt, left_duration)?;
            return Ok(xml_datetime_value(right.type_code, result));
        }
        if let Some(right_date) = as_date(right) {
            let result = add_date_year_month(right_date, left_duration)?;
            return Ok(xml_date_value(right.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Add, left, right));
    }

    if let Some(left_duration) = as_day_time_duration(left) {
        if let Some(right_duration) = as_day_time_duration(right) {
            let total =
                day_time_total_seconds(left_duration)? + day_time_total_seconds(right_duration)?;
            return Ok(xml_day_time_duration_value(
                day_time_from_seconds(total)?,
            ));
        }
        if let Some(right_dt) = as_datetime(right) {
            let result = add_datetime_day_time(right_dt, left_duration)?;
            return Ok(xml_datetime_value(right.type_code, result));
        }
        if let Some(right_date) = as_date(right) {
            let result = add_date_day_time(right_date, left_duration)?;
            return Ok(xml_date_value(right.type_code, result));
        }
        if let Some(right_time) = as_time(right) {
            let result = add_time_day_time(right_time, left_duration)?;
            return Ok(xml_time_value(right.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Add, left, right));
    }

    Err(unsupported_operator(BinaryOpKind::Add, left, right))
}

fn eval_temporal_sub(left: &XmlValue, right: &XmlValue) -> Result<XmlValue, XPathError> {
    if let Some(left_dt) = as_datetime(left) {
        if let Some(right_dt) = as_datetime(right) {
            let result = diff_datetime(left_dt, right_dt)?;
            return Ok(xml_day_time_duration_value(result));
        }
        if let Some(duration) = as_year_month_duration(right) {
            let result = add_datetime_year_month(left_dt, &negate_year_month_duration(duration))?;
            return Ok(xml_datetime_value(left.type_code, result));
        }
        if let Some(duration) = as_day_time_duration(right) {
            let result = add_datetime_day_time(left_dt, &negate_day_time_duration(duration))?;
            return Ok(xml_datetime_value(left.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Sub, left, right));
    }

    if let Some(left_date) = as_date(left) {
        if let Some(right_date) = as_date(right) {
            let result = diff_date(left_date, right_date)?;
            return Ok(xml_day_time_duration_value(result));
        }
        if let Some(duration) = as_year_month_duration(right) {
            let result = add_date_year_month(left_date, &negate_year_month_duration(duration))?;
            return Ok(xml_date_value(left.type_code, result));
        }
        if let Some(duration) = as_day_time_duration(right) {
            let result = add_date_day_time(left_date, &negate_day_time_duration(duration))?;
            return Ok(xml_date_value(left.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Sub, left, right));
    }

    if let Some(left_time) = as_time(left) {
        if let Some(right_time) = as_time(right) {
            let result = diff_time(left_time, right_time)?;
            return Ok(xml_day_time_duration_value(result));
        }
        if let Some(duration) = as_day_time_duration(right) {
            let result = add_time_day_time(left_time, &negate_day_time_duration(duration))?;
            return Ok(xml_time_value(left.type_code, result));
        }
        return Err(unsupported_operator(BinaryOpKind::Sub, left, right));
    }

    if let Some(left_duration) = as_year_month_duration(left) {
        if let Some(right_duration) = as_year_month_duration(right) {
            let total = year_month_total_months(left_duration)
                - year_month_total_months(right_duration);
            return Ok(xml_year_month_duration_value(
                year_month_from_months(total)?,
            ));
        }
        return Err(unsupported_operator(BinaryOpKind::Sub, left, right));
    }

    if let Some(left_duration) = as_day_time_duration(left) {
        if let Some(right_duration) = as_day_time_duration(right) {
            let total =
                day_time_total_seconds(left_duration)? - day_time_total_seconds(right_duration)?;
            return Ok(xml_day_time_duration_value(
                day_time_from_seconds(total)?,
            ));
        }
        return Err(unsupported_operator(BinaryOpKind::Sub, left, right));
    }

    Err(unsupported_operator(BinaryOpKind::Sub, left, right))
}

fn eval_temporal_mul(left: &XmlValue, right: &XmlValue) -> Result<XmlValue, XPathError> {
    if let Some(duration) = as_year_month_duration(left) {
        if right.type_code.is_numeric() {
            let factor = numeric_to_f64(right)?;
            return Ok(xml_year_month_duration_value(
                year_month_mul_numeric(duration, factor)?,
            ));
        }
        return Err(unsupported_operator(BinaryOpKind::Mul, left, right));
    }

    if let Some(duration) = as_day_time_duration(left) {
        if right.type_code.is_numeric() {
            let factor = numeric_to_decimal(right)?;
            return Ok(xml_day_time_duration_value(
                day_time_mul_numeric(duration, factor)?,
            ));
        }
        return Err(unsupported_operator(BinaryOpKind::Mul, left, right));
    }

    if left.type_code.is_numeric() {
        if let Some(duration) = as_year_month_duration(right) {
            let factor = numeric_to_f64(left)?;
            return Ok(xml_year_month_duration_value(
                year_month_mul_numeric(duration, factor)?,
            ));
        }
        if let Some(duration) = as_day_time_duration(right) {
            let factor = numeric_to_decimal(left)?;
            return Ok(xml_day_time_duration_value(
                day_time_mul_numeric(duration, factor)?,
            ));
        }
    }

    Err(unsupported_operator(BinaryOpKind::Mul, left, right))
}

fn eval_temporal_div(left: &XmlValue, right: &XmlValue) -> Result<XmlValue, XPathError> {
    if let Some(duration) = as_year_month_duration(left) {
        if right.type_code.is_numeric() {
            let divisor = numeric_to_f64(right)?;
            return Ok(xml_year_month_duration_value(
                year_month_div_numeric(duration, divisor)?,
            ));
        }
        if let Some(right_duration) = as_year_month_duration(right) {
            let ratio = year_month_div_duration(duration, right_duration)?;
            return Ok(XmlValue::decimal(ratio));
        }
        return Err(unsupported_operator(BinaryOpKind::Div, left, right));
    }

    if let Some(duration) = as_day_time_duration(left) {
        if right.type_code.is_numeric() {
            let divisor = numeric_to_decimal(right)?;
            return Ok(xml_day_time_duration_value(
                day_time_div_numeric(duration, divisor)?,
            ));
        }
        if let Some(right_duration) = as_day_time_duration(right) {
            let ratio = day_time_div_duration(duration, right_duration)?;
            return Ok(XmlValue::decimal(ratio));
        }
        return Err(unsupported_operator(BinaryOpKind::Div, left, right));
    }

    Err(unsupported_operator(BinaryOpKind::Div, left, right))
}

fn numeric_eq(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let (left_val, right_val, class) = promote_numeric(left, right)?;
    Ok(match class {
        NumericClass::Float => {
            let (l, r) = float_pair(left_val, right_val)?;
            l == r
        }
        NumericClass::Double => {
            let (l, r) = double_pair(left_val, right_val)?;
            l == r
        }
        NumericClass::Decimal => {
            let (l, r) = decimal_pair(left_val, right_val)?;
            l == r
        }
        _ => {
            let (l, r) = integer_pair(left_val, right_val)?;
            l == r
        }
    })
}

fn numeric_gt(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let (left_val, right_val, class) = promote_numeric(left, right)?;
    Ok(match class {
        NumericClass::Float => {
            let (l, r) = float_pair(left_val, right_val)?;
            l > r
        }
        NumericClass::Double => {
            let (l, r) = double_pair(left_val, right_val)?;
            l > r
        }
        NumericClass::Decimal => {
            let (l, r) = decimal_pair(left_val, right_val)?;
            l > r
        }
        _ => {
            let (l, r) = integer_pair(left_val, right_val)?;
            l > r
        }
    })
}

fn eval_numeric_unary(value: &XmlValue) -> Result<XmlValue, XPathError> {
    let class = numeric_class(value.type_code)
        .ok_or_else(|| XPathError::internal("Unary operator requires numeric operand"))?;

    let result_type = unary_result_type(class);
    let value = to_numeric_value(value, class)?;

    let result = match value {
        NumericValue::Integer(v) => NumericValue::Integer(-v),
        NumericValue::Decimal(v) => NumericValue::Decimal(-v),
        NumericValue::Float(v) => NumericValue::Float(-v),
        NumericValue::Double(v) => NumericValue::Double(-v),
    };

    Ok(numeric_to_xml_value(result, result_type))
}

fn eval_numeric_binary(
    op: BinaryOpKind,
    left: &XmlValue,
    right: &XmlValue,
) -> Result<XmlValue, XPathError> {
    let (left_val, right_val, class) = promote_numeric(left, right)?;
    let result_type = binary_result_type(op, class);

    match op {
        BinaryOpKind::Add => Ok(numeric_add(left_val, right_val, result_type)?),
        BinaryOpKind::Sub => Ok(numeric_sub(left_val, right_val, result_type)?),
        BinaryOpKind::Mul => Ok(numeric_mul(left_val, right_val, result_type)?),
        BinaryOpKind::Div => Ok(numeric_div(left_val, right_val, class, result_type)?),
        BinaryOpKind::IDiv => Ok(numeric_idiv(left_val, right_val)?),
        BinaryOpKind::Mod => Ok(numeric_mod(left_val, right_val, result_type)?),
        _ => Err(XPathError::internal("Unsupported numeric operator")),
    }
}

fn numeric_add(
    left: NumericValue,
    right: NumericValue,
    result_type: XmlTypeCode,
) -> Result<XmlValue, XPathError> {
    let result = match (left, right) {
        (NumericValue::Integer(l), NumericValue::Integer(r)) => {
            NumericValue::Integer(l + r)
        }
        (NumericValue::Decimal(l), NumericValue::Decimal(r)) => NumericValue::Decimal(l + r),
        (NumericValue::Float(l), NumericValue::Float(r)) => NumericValue::Float(l + r),
        (NumericValue::Double(l), NumericValue::Double(r)) => NumericValue::Double(l + r),
        _ => return Err(XPathError::internal("Numeric add type mismatch")),
    };
    Ok(numeric_to_xml_value(result, result_type))
}

fn numeric_sub(
    left: NumericValue,
    right: NumericValue,
    result_type: XmlTypeCode,
) -> Result<XmlValue, XPathError> {
    let result = match (left, right) {
        (NumericValue::Integer(l), NumericValue::Integer(r)) => {
            NumericValue::Integer(l - r)
        }
        (NumericValue::Decimal(l), NumericValue::Decimal(r)) => NumericValue::Decimal(l - r),
        (NumericValue::Float(l), NumericValue::Float(r)) => NumericValue::Float(l - r),
        (NumericValue::Double(l), NumericValue::Double(r)) => NumericValue::Double(l - r),
        _ => return Err(XPathError::internal("Numeric sub type mismatch")),
    };
    Ok(numeric_to_xml_value(result, result_type))
}

fn numeric_mul(
    left: NumericValue,
    right: NumericValue,
    result_type: XmlTypeCode,
) -> Result<XmlValue, XPathError> {
    let result = match (left, right) {
        (NumericValue::Integer(l), NumericValue::Integer(r)) => {
            NumericValue::Integer(l * r)
        }
        (NumericValue::Decimal(l), NumericValue::Decimal(r)) => NumericValue::Decimal(l * r),
        (NumericValue::Float(l), NumericValue::Float(r)) => NumericValue::Float(l * r),
        (NumericValue::Double(l), NumericValue::Double(r)) => NumericValue::Double(l * r),
        _ => return Err(XPathError::internal("Numeric mul type mismatch")),
    };
    Ok(numeric_to_xml_value(result, result_type))
}

fn numeric_div(
    left: NumericValue,
    right: NumericValue,
    class: NumericClass,
    result_type: XmlTypeCode,
) -> Result<XmlValue, XPathError> {
    let result = match (left, right) {
        (NumericValue::Integer(l), NumericValue::Integer(r)) => {
            let left_dec = decimal_from_bigint(&l)?;
            let right_dec = decimal_from_bigint(&r)?;
            NumericValue::Decimal(left_dec / right_dec)
        }
        (NumericValue::Decimal(l), NumericValue::Decimal(r)) => NumericValue::Decimal(l / r),
        (NumericValue::Float(l), NumericValue::Float(r)) => NumericValue::Float(l / r),
        (NumericValue::Double(l), NumericValue::Double(r)) => NumericValue::Double(l / r),
        _ => return Err(XPathError::internal("Numeric div type mismatch")),
    };

    let result_type = match class {
        NumericClass::Decimal | NumericClass::Float | NumericClass::Double => result_type,
        _ => XmlTypeCode::Decimal,
    };

    Ok(numeric_to_xml_value(result, result_type))
}

fn numeric_idiv(left: NumericValue, right: NumericValue) -> Result<XmlValue, XPathError> {
    let result = match (left, right) {
        (NumericValue::Integer(l), NumericValue::Integer(r)) => NumericValue::Integer(l / r),
        (NumericValue::Decimal(l), NumericValue::Decimal(r)) => {
            let q = (l / r).trunc();
            NumericValue::Integer(decimal_to_bigint(&q)?)
        }
        (NumericValue::Float(l), NumericValue::Float(r)) => {
            NumericValue::Integer(BigInt::from((l / r).trunc() as i64))
        }
        (NumericValue::Double(l), NumericValue::Double(r)) => {
            NumericValue::Integer(BigInt::from((l / r).trunc() as i64))
        }
        _ => return Err(XPathError::internal("Numeric idiv type mismatch")),
    };

    Ok(numeric_to_xml_value(result, XmlTypeCode::Integer))
}

fn numeric_mod(
    left: NumericValue,
    right: NumericValue,
    result_type: XmlTypeCode,
) -> Result<XmlValue, XPathError> {
    let result = match (left, right) {
        (NumericValue::Integer(l), NumericValue::Integer(r)) => {
            NumericValue::Integer(l % r)
        }
        (NumericValue::Decimal(l), NumericValue::Decimal(r)) => NumericValue::Decimal(l % r),
        (NumericValue::Float(l), NumericValue::Float(r)) => NumericValue::Float(l % r),
        (NumericValue::Double(l), NumericValue::Double(r)) => NumericValue::Double(l % r),
        _ => return Err(XPathError::internal("Numeric mod type mismatch")),
    };
    Ok(numeric_to_xml_value(result, result_type))
}

fn promote_numeric(
    left: &XmlValue,
    right: &XmlValue,
) -> Result<(NumericValue, NumericValue, NumericClass), XPathError> {
    let left_class = numeric_class(left.type_code)
        .ok_or_else(|| XPathError::internal("Left operand not numeric"))?;
    let right_class = numeric_class(right.type_code)
        .ok_or_else(|| XPathError::internal("Right operand not numeric"))?;

    let promotion = numeric_promotion(left_class, right_class);
    let target_class = match promotion {
        Promote::Left => left_class,
        Promote::Right => right_class,
        Promote::None => left_class,
    };

    let left_val = to_numeric_value(left, target_class)?;
    let right_val = to_numeric_value(right, target_class)?;

    Ok((left_val, right_val, target_class))
}

fn numeric_class(code: XmlTypeCode) -> Option<NumericClass> {
    match code {
        XmlTypeCode::Byte => Some(NumericClass::Byte),
        XmlTypeCode::UnsignedByte => Some(NumericClass::UnsignedByte),
        XmlTypeCode::Short => Some(NumericClass::Short),
        XmlTypeCode::UnsignedShort => Some(NumericClass::UnsignedShort),
        XmlTypeCode::Int => Some(NumericClass::Int),
        XmlTypeCode::UnsignedInt => Some(NumericClass::UnsignedInt),
        XmlTypeCode::Long => Some(NumericClass::Long),
        XmlTypeCode::UnsignedLong => Some(NumericClass::UnsignedLong),
        XmlTypeCode::Integer
        | XmlTypeCode::NonPositiveInteger
        | XmlTypeCode::NegativeInteger
        | XmlTypeCode::NonNegativeInteger
        | XmlTypeCode::PositiveInteger => Some(NumericClass::Integer),
        XmlTypeCode::Decimal => Some(NumericClass::Decimal),
        XmlTypeCode::Float => Some(NumericClass::Float),
        XmlTypeCode::Double => Some(NumericClass::Double),
        _ => None,
    }
}

fn numeric_precedence(class: NumericClass) -> u8 {
    match class {
        NumericClass::Byte => 0,
        NumericClass::UnsignedByte => 1,
        NumericClass::Short => 2,
        NumericClass::UnsignedShort => 3,
        NumericClass::Int => 4,
        NumericClass::UnsignedInt => 5,
        NumericClass::Long => 6,
        NumericClass::UnsignedLong => 7,
        NumericClass::Integer => 8,
        NumericClass::Decimal => 9,
        NumericClass::Float => 10,
        NumericClass::Double => 11,
    }
}

fn numeric_promotion(left: NumericClass, right: NumericClass) -> Promote {
    let left_rank = numeric_precedence(left);
    let right_rank = numeric_precedence(right);

    match left_rank.cmp(&right_rank) {
        Ordering::Less => Promote::Right,
        Ordering::Greater => Promote::Left,
        Ordering::Equal => Promote::None,
    }
}

fn binary_result_type(op: BinaryOpKind, class: NumericClass) -> XmlTypeCode {
    match op {
        BinaryOpKind::Div => div_result_type(class),
        BinaryOpKind::IDiv => XmlTypeCode::Integer,
        BinaryOpKind::Add | BinaryOpKind::Sub | BinaryOpKind::Mul | BinaryOpKind::Mod => {
            arithmetic_result_type(class)
        }
        _ => arithmetic_result_type(class),
    }
}

fn unary_result_type(class: NumericClass) -> XmlTypeCode {
    match class {
        NumericClass::Byte
        | NumericClass::UnsignedByte
        | NumericClass::Short
        | NumericClass::UnsignedShort => XmlTypeCode::Int,
        NumericClass::UnsignedInt => XmlTypeCode::Long,
        NumericClass::UnsignedLong => XmlTypeCode::Integer,
        NumericClass::Int => XmlTypeCode::Int,
        NumericClass::Long => XmlTypeCode::Long,
        NumericClass::Integer => XmlTypeCode::Integer,
        NumericClass::Decimal => XmlTypeCode::Decimal,
        NumericClass::Float => XmlTypeCode::Float,
        NumericClass::Double => XmlTypeCode::Double,
    }
}

fn arithmetic_result_type(class: NumericClass) -> XmlTypeCode {
    match class {
        NumericClass::Byte
        | NumericClass::UnsignedByte
        | NumericClass::Short
        | NumericClass::UnsignedShort => XmlTypeCode::Int,
        NumericClass::UnsignedInt => XmlTypeCode::UnsignedInt,
        NumericClass::Int => XmlTypeCode::Int,
        NumericClass::Long => XmlTypeCode::Long,
        NumericClass::UnsignedLong => XmlTypeCode::Integer,
        NumericClass::Integer => XmlTypeCode::Integer,
        NumericClass::Decimal => XmlTypeCode::Decimal,
        NumericClass::Float => XmlTypeCode::Float,
        NumericClass::Double => XmlTypeCode::Double,
    }
}

fn div_result_type(class: NumericClass) -> XmlTypeCode {
    match class {
        NumericClass::Decimal => XmlTypeCode::Decimal,
        NumericClass::Float => XmlTypeCode::Float,
        NumericClass::Double => XmlTypeCode::Double,
        _ => XmlTypeCode::Decimal,
    }
}

fn is_integer_class(class: NumericClass) -> bool {
    matches!(
        class,
        NumericClass::Byte
            | NumericClass::UnsignedByte
            | NumericClass::Short
            | NumericClass::UnsignedShort
            | NumericClass::Int
            | NumericClass::UnsignedInt
            | NumericClass::Long
            | NumericClass::UnsignedLong
            | NumericClass::Integer
    )
}

fn to_integer_value(value: &XmlValue) -> Result<BigInt, XPathError> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Integer(v)) => Ok(v.clone()),
        _ => Err(XPathError::internal("Expected integer value")),
    }
}

fn to_numeric_value(value: &XmlValue, class: NumericClass) -> Result<NumericValue, XPathError> {
    match class {
        NumericClass::Decimal => {
            let decimal = value
                .as_decimal()
                .ok_or_else(|| XPathError::internal("Expected decimal value"))?;
            Ok(NumericValue::Decimal(decimal))
        }
        NumericClass::Float => {
            let val = value
                .as_double()
                .ok_or_else(|| XPathError::internal("Expected numeric value"))?;
            Ok(NumericValue::Float(val as f32))
        }
        NumericClass::Double => {
            let val = value
                .as_double()
                .ok_or_else(|| XPathError::internal("Expected numeric value"))?;
            Ok(NumericValue::Double(val))
        }
        _ => Ok(NumericValue::Integer(to_integer_value(value)?)),
    }
}

fn decimal_from_bigint(value: &BigInt) -> Result<Decimal, XPathError> {
    value
        .to_string()
        .parse::<Decimal>()
        .map_err(|_| XPathError::internal("Failed to convert integer to decimal"))
}

fn decimal_to_bigint(value: &Decimal) -> Result<BigInt, XPathError> {
    value
        .trunc()
        .to_string()
        .parse::<BigInt>()
        .map_err(|_| XPathError::internal("Failed to convert decimal to integer"))
}

fn numeric_to_xml_value(value: NumericValue, type_code: XmlTypeCode) -> XmlValue {
    let value = match value {
        NumericValue::Integer(v) => XmlValueKind::Atomic(XmlAtomicValue::Integer(v)),
        NumericValue::Decimal(v) => XmlValueKind::Atomic(XmlAtomicValue::Decimal(v)),
        NumericValue::Float(v) => XmlValueKind::Atomic(XmlAtomicValue::Float(v)),
        NumericValue::Double(v) => XmlValueKind::Atomic(XmlAtomicValue::Double(v)),
    };

    XmlValue {
        type_code,
        schema_type: None,
        value,
    }
}

fn integer_pair(
    left: NumericValue,
    right: NumericValue,
) -> Result<(BigInt, BigInt), XPathError> {
    match (left, right) {
        (NumericValue::Integer(l), NumericValue::Integer(r)) => Ok((l, r)),
        _ => Err(XPathError::internal("Expected integer values")),
    }
}

fn decimal_pair(
    left: NumericValue,
    right: NumericValue,
) -> Result<(Decimal, Decimal), XPathError> {
    match (left, right) {
        (NumericValue::Decimal(l), NumericValue::Decimal(r)) => Ok((l, r)),
        _ => Err(XPathError::internal("Expected decimal values")),
    }
}

fn float_pair(
    left: NumericValue,
    right: NumericValue,
) -> Result<(f32, f32), XPathError> {
    match (left, right) {
        (NumericValue::Float(l), NumericValue::Float(r)) => Ok((l, r)),
        _ => Err(XPathError::internal("Expected float values")),
    }
}

fn double_pair(
    left: NumericValue,
    right: NumericValue,
) -> Result<(f64, f64), XPathError> {
    match (left, right) {
        (NumericValue::Double(l), NumericValue::Double(r)) => Ok((l, r)),
        _ => Err(XPathError::internal("Expected double values")),
    }
}

fn xml_datetime_value(type_code: XmlTypeCode, value: DateTimeValue) -> XmlValue {
    XmlValue {
        type_code,
        schema_type: None,
        value: XmlValueKind::Atomic(XmlAtomicValue::DateTime(value)),
    }
}

fn xml_date_value(type_code: XmlTypeCode, value: DateValue) -> XmlValue {
    XmlValue {
        type_code,
        schema_type: None,
        value: XmlValueKind::Atomic(XmlAtomicValue::Date(value)),
    }
}

fn xml_time_value(type_code: XmlTypeCode, value: TimeValue) -> XmlValue {
    XmlValue {
        type_code,
        schema_type: None,
        value: XmlValueKind::Atomic(XmlAtomicValue::Time(value)),
    }
}

fn xml_year_month_duration_value(value: YearMonthDurationValue) -> XmlValue {
    XmlValue {
        type_code: XmlTypeCode::YearMonthDuration,
        schema_type: None,
        value: XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(value)),
    }
}

fn xml_day_time_duration_value(value: DayTimeDurationValue) -> XmlValue {
    XmlValue {
        type_code: XmlTypeCode::DayTimeDuration,
        schema_type: None,
        value: XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(value)),
    }
}

fn is_temporal_type(code: XmlTypeCode) -> bool {
    matches!(
        code,
        XmlTypeCode::DateTime
            | XmlTypeCode::DateTimeStamp
            | XmlTypeCode::Date
            | XmlTypeCode::Time
            | XmlTypeCode::Duration
            | XmlTypeCode::YearMonthDuration
            | XmlTypeCode::DayTimeDuration
    )
}

fn is_date_time_code(code: XmlTypeCode) -> bool {
    matches!(code, XmlTypeCode::DateTime | XmlTypeCode::DateTimeStamp)
}

fn is_duration_code(code: XmlTypeCode) -> bool {
    matches!(
        code,
        XmlTypeCode::Duration | XmlTypeCode::YearMonthDuration | XmlTypeCode::DayTimeDuration
    )
}

fn as_datetime(value: &XmlValue) -> Option<&DateTimeValue> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::DateTime(v)) => Some(v),
        _ => None,
    }
}

fn as_date(value: &XmlValue) -> Option<&DateValue> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Date(v)) => Some(v),
        _ => None,
    }
}

fn as_time(value: &XmlValue) -> Option<&TimeValue> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Time(v)) => Some(v),
        _ => None,
    }
}

fn as_duration(value: &XmlValue) -> Option<&DurationValue> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Duration(v)) => Some(v),
        _ => None,
    }
}

fn as_year_month_duration(value: &XmlValue) -> Option<&YearMonthDurationValue> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(v)) => Some(v),
        _ => None,
    }
}

fn as_day_time_duration(value: &XmlValue) -> Option<&DayTimeDurationValue> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(v)) => Some(v),
        _ => None,
    }
}

fn duration_parts(value: &XmlValue) -> Result<Option<(i64, Decimal)>, XPathError> {
    if let Some(duration) = as_duration(value) {
        let months = duration_total_months(duration);
        let seconds = duration_total_seconds(duration)?;
        return Ok(Some((months, seconds)));
    }
    if let Some(duration) = as_year_month_duration(value) {
        return Ok(Some((year_month_total_months(duration), Decimal::ZERO)));
    }
    if let Some(duration) = as_day_time_duration(value) {
        let seconds = day_time_total_seconds(duration)?;
        return Ok(Some((0, seconds)));
    }
    Ok(None)
}

fn numeric_to_f64(value: &XmlValue) -> Result<f64, XPathError> {
    let val = value
        .as_double()
        .ok_or_else(|| XPathError::internal("Expected numeric value"))?;
    if !val.is_finite() {
        return Err(XPathError::internal("Numeric value is not finite"));
    }
    Ok(val)
}

fn numeric_to_decimal(value: &XmlValue) -> Result<Decimal, XPathError> {
    match value.type_code {
        XmlTypeCode::Float | XmlTypeCode::Double => {
            let val = numeric_to_f64(value)?;
            Decimal::from_f64(val)
                .ok_or_else(|| XPathError::internal("Failed to convert float to decimal"))
        }
        _ => value
            .as_decimal()
            .ok_or_else(|| XPathError::internal("Expected decimal value")),
    }
}

fn compare_datetime_eq(left: &DateTimeValue, right: &DateTimeValue) -> Result<bool, XPathError> {
    let left_inst = datetime_instant_for_compare(left)?;
    let right_inst = datetime_instant_for_compare(right)?;
    Ok(left_inst == right_inst)
}

fn compare_datetime_gt(left: &DateTimeValue, right: &DateTimeValue) -> Result<bool, XPathError> {
    let left_inst = datetime_instant_for_compare(left)?;
    let right_inst = datetime_instant_for_compare(right)?;
    Ok(left_inst > right_inst)
}

fn compare_date_eq(left: &DateValue, right: &DateValue) -> Result<bool, XPathError> {
    let left_inst = date_instant_for_compare(left)?;
    let right_inst = date_instant_for_compare(right)?;
    Ok(left_inst == right_inst)
}

fn compare_date_gt(left: &DateValue, right: &DateValue) -> Result<bool, XPathError> {
    let left_inst = date_instant_for_compare(left)?;
    let right_inst = date_instant_for_compare(right)?;
    Ok(left_inst > right_inst)
}

fn compare_time_eq(left: &TimeValue, right: &TimeValue) -> Result<bool, XPathError> {
    let left_inst = time_seconds_for_compare(left)?;
    let right_inst = time_seconds_for_compare(right)?;
    Ok(left_inst == right_inst)
}

fn compare_time_gt(left: &TimeValue, right: &TimeValue) -> Result<bool, XPathError> {
    let left_inst = time_seconds_for_compare(left)?;
    let right_inst = time_seconds_for_compare(right)?;
    Ok(left_inst > right_inst)
}

fn datetime_instant_for_compare(value: &DateTimeValue) -> Result<Decimal, XPathError> {
    let instant = datetime_to_instant(value)?;
    apply_timezone_offset(instant, value.timezone)
}

fn date_instant_for_compare(value: &DateValue) -> Result<Decimal, XPathError> {
    let instant = date_to_instant(value)?;
    apply_timezone_offset(instant, value.timezone)
}

fn time_seconds_for_compare(value: &TimeValue) -> Result<Decimal, XPathError> {
    let seconds = time_to_seconds(value)?;
    apply_timezone_offset(seconds, value.timezone)
}

fn apply_timezone_offset(
    instant: Decimal,
    timezone: Option<TimezoneOffset>,
) -> Result<Decimal, XPathError> {
    let offset = timezone.unwrap_or_else(implicit_timezone_offset);
    let offset_minutes = decimal_from_i64(offset.0 as i64)?;
    Ok(instant - offset_minutes * Decimal::from(60))
}

fn implicit_timezone_offset() -> TimezoneOffset {
    let seconds = Local::now().offset().local_minus_utc();
    TimezoneOffset((seconds / 60) as i16)
}

fn add_datetime_year_month(
    value: &DateTimeValue,
    duration: &YearMonthDurationValue,
) -> Result<DateTimeValue, XPathError> {
    let delta = year_month_total_months(duration);
    let (year, month, day) = add_months_to_date(value.year, value.month, value.day, delta)?;
    Ok(DateTimeValue {
        year,
        month,
        day,
        hour: value.hour,
        minute: value.minute,
        second: value.second,
        timezone: value.timezone,
    })
}

fn add_date_year_month(
    value: &DateValue,
    duration: &YearMonthDurationValue,
) -> Result<DateValue, XPathError> {
    let delta = year_month_total_months(duration);
    let (year, month, day) = add_months_to_date(value.year, value.month, value.day, delta)?;
    Ok(DateValue {
        year,
        month,
        day,
        timezone: value.timezone,
    })
}

fn add_datetime_day_time(
    value: &DateTimeValue,
    duration: &DayTimeDurationValue,
) -> Result<DateTimeValue, XPathError> {
    let instant = datetime_to_instant(value)?;
    let delta = day_time_total_seconds(duration)?;
    instant_to_datetime(instant + delta, value.timezone)
}

fn add_date_day_time(
    value: &DateValue,
    duration: &DayTimeDurationValue,
) -> Result<DateValue, XPathError> {
    let instant = date_to_instant(value)?;
    let delta = day_time_total_seconds(duration)?;
    instant_to_date(instant + delta, value.timezone)
}

fn add_time_day_time(
    value: &TimeValue,
    duration: &DayTimeDurationValue,
) -> Result<TimeValue, XPathError> {
    let seconds = time_to_seconds(value)?;
    let delta = day_time_total_seconds(duration)?;
    let normalized = normalize_seconds_in_day(seconds + delta)?;
    let (hour, minute, second) = time_components_from_seconds(normalized)?;
    Ok(TimeValue {
        hour,
        minute,
        second,
        timezone: value.timezone,
    })
}

fn diff_datetime(
    left: &DateTimeValue,
    right: &DateTimeValue,
) -> Result<DayTimeDurationValue, XPathError> {
    let left_inst = datetime_instant_for_compare(left)?;
    let right_inst = datetime_instant_for_compare(right)?;
    day_time_from_seconds(left_inst - right_inst)
}

fn diff_date(left: &DateValue, right: &DateValue) -> Result<DayTimeDurationValue, XPathError> {
    let left_inst = date_instant_for_compare(left)?;
    let right_inst = date_instant_for_compare(right)?;
    day_time_from_seconds(left_inst - right_inst)
}

fn diff_time(left: &TimeValue, right: &TimeValue) -> Result<DayTimeDurationValue, XPathError> {
    let left_inst = time_seconds_for_compare(left)?;
    let right_inst = time_seconds_for_compare(right)?;
    day_time_from_seconds(left_inst - right_inst)
}

fn year_month_total_months(value: &YearMonthDurationValue) -> i64 {
    let total = value.years as i64 * 12 + value.months as i64;
    if value.negative {
        -total
    } else {
        total
    }
}

fn duration_total_months(value: &DurationValue) -> i64 {
    let total = value.years as i64 * 12 + value.months as i64;
    if value.negative {
        -total
    } else {
        total
    }
}

fn day_time_total_seconds(value: &DayTimeDurationValue) -> Result<Decimal, XPathError> {
    let days = decimal_from_i64(value.days as i64)?;
    let hours = decimal_from_i64(value.hours as i64)?;
    let minutes = decimal_from_i64(value.minutes as i64)?;
    let total = days * Decimal::from(86_400)
        + hours * Decimal::from(3_600)
        + minutes * Decimal::from(60)
        + value.seconds;
    Ok(if value.negative { -total } else { total })
}

fn duration_total_seconds(value: &DurationValue) -> Result<Decimal, XPathError> {
    let days = decimal_from_i64(value.days as i64)?;
    let hours = decimal_from_i64(value.hours as i64)?;
    let minutes = decimal_from_i64(value.minutes as i64)?;
    let total = days * Decimal::from(86_400)
        + hours * Decimal::from(3_600)
        + minutes * Decimal::from(60)
        + value.seconds;
    Ok(if value.negative { -total } else { total })
}

fn negate_year_month_duration(value: &YearMonthDurationValue) -> YearMonthDurationValue {
    let negative = if value.years == 0 && value.months == 0 {
        false
    } else {
        !value.negative
    };
    YearMonthDurationValue {
        negative,
        years: value.years,
        months: value.months,
    }
}

fn negate_day_time_duration(value: &DayTimeDurationValue) -> DayTimeDurationValue {
    let negative = if value.days == 0
        && value.hours == 0
        && value.minutes == 0
        && value.seconds.is_zero()
    {
        false
    } else {
        !value.negative
    };
    DayTimeDurationValue {
        negative,
        days: value.days,
        hours: value.hours,
        minutes: value.minutes,
        seconds: value.seconds,
    }
}

fn year_month_from_months(months: i64) -> Result<YearMonthDurationValue, XPathError> {
    let negative = months < 0;
    let abs_months = months.abs();
    let years = abs_months / 12;
    let months = abs_months % 12;
    let years = u32::try_from(years)
        .map_err(|_| XPathError::internal("yearMonthDuration years out of range"))?;
    let months = u32::try_from(months)
        .map_err(|_| XPathError::internal("yearMonthDuration months out of range"))?;
    Ok(YearMonthDurationValue {
        negative,
        years,
        months,
    })
}

fn day_time_from_seconds(seconds: Decimal) -> Result<DayTimeDurationValue, XPathError> {
    let negative = seconds.is_sign_negative();
    let abs = if negative { -seconds } else { seconds };
    let seconds_per_day = decimal_from_i64(86_400)?;
    let days = (abs / seconds_per_day).floor();
    let mut remainder = abs - days * seconds_per_day;
    let hours = (remainder / Decimal::from(3_600)).floor();
    remainder -= hours * Decimal::from(3_600);
    let minutes = (remainder / Decimal::from(60)).floor();
    let seconds = remainder - minutes * Decimal::from(60);
    let days = decimal_to_u32(days, "days")?;
    let hours = decimal_to_u32(hours, "hours")?;
    let minutes = decimal_to_u32(minutes, "minutes")?;
    Ok(DayTimeDurationValue {
        negative,
        days,
        hours,
        minutes,
        seconds,
    })
}

fn year_month_mul_numeric(
    value: &YearMonthDurationValue,
    factor: f64,
) -> Result<YearMonthDurationValue, XPathError> {
    if !factor.is_finite() {
        return Err(XPathError::internal("Numeric value is not finite"));
    }
    let months = year_month_total_months(value) as f64 * factor;
    let rounded = (months + 0.5).floor();
    year_month_from_months(rounded as i64)
}

fn year_month_div_numeric(
    value: &YearMonthDurationValue,
    divisor: f64,
) -> Result<YearMonthDurationValue, XPathError> {
    if divisor == 0.0 {
        return Err(XPathError::internal("Division by zero"));
    }
    if !divisor.is_finite() {
        return Err(XPathError::internal("Numeric value is not finite"));
    }
    let months = year_month_total_months(value) as f64 / divisor;
    let rounded = (months + 0.5).floor();
    year_month_from_months(rounded as i64)
}

fn year_month_div_duration(
    left: &YearMonthDurationValue,
    right: &YearMonthDurationValue,
) -> Result<Decimal, XPathError> {
    let left_months = year_month_total_months(left);
    let right_months = year_month_total_months(right);
    if right_months == 0 {
        return Err(XPathError::internal("Division by zero"));
    }
    let left = decimal_from_i64(left_months)?;
    let right = decimal_from_i64(right_months)?;
    Ok(left / right)
}

fn day_time_mul_numeric(
    value: &DayTimeDurationValue,
    factor: Decimal,
) -> Result<DayTimeDurationValue, XPathError> {
    let total = day_time_total_seconds(value)? * factor;
    day_time_from_seconds(total)
}

fn day_time_div_numeric(
    value: &DayTimeDurationValue,
    divisor: Decimal,
) -> Result<DayTimeDurationValue, XPathError> {
    if divisor.is_zero() {
        return Err(XPathError::internal("Division by zero"));
    }
    let total = day_time_total_seconds(value)? / divisor;
    day_time_from_seconds(total)
}

fn day_time_div_duration(
    left: &DayTimeDurationValue,
    right: &DayTimeDurationValue,
) -> Result<Decimal, XPathError> {
    let left_seconds = day_time_total_seconds(left)?;
    let right_seconds = day_time_total_seconds(right)?;
    if right_seconds.is_zero() {
        return Err(XPathError::internal("Division by zero"));
    }
    Ok(left_seconds / right_seconds)
}

fn add_months_to_date(
    year: i32,
    month: u8,
    day: u8,
    delta_months: i64,
) -> Result<(i32, u8, u8), XPathError> {
    let month_index = month as i64 - 1;
    let total = year as i64 * 12 + month_index + delta_months;
    let new_year = total.div_euclid(12);
    let new_month = total.rem_euclid(12) + 1;
    let year = i32::try_from(new_year)
        .map_err(|_| XPathError::internal("Year out of range"))?;
    let month = u8::try_from(new_month)
        .map_err(|_| XPathError::internal("Month out of range"))?;
    let max_day = days_in_month(year, month)?;
    let day = day.min(max_day);
    Ok((year, month, day))
}

fn days_in_month(year: i32, month: u8) -> Result<u8, XPathError> {
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => return Err(XPathError::internal("Invalid month value")),
    };
    Ok(days)
}

fn is_leap_year(year: i32) -> bool {
    let year = year as i64;
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

fn datetime_to_instant(value: &DateTimeValue) -> Result<Decimal, XPathError> {
    let days = days_from_civil(value.year, value.month, value.day);
    let seconds = time_to_seconds_components(value.hour, value.minute, value.second)?;
    Ok(decimal_from_i64(days)? * Decimal::from(86_400) + seconds)
}

fn date_to_instant(value: &DateValue) -> Result<Decimal, XPathError> {
    let days = days_from_civil(value.year, value.month, value.day);
    Ok(decimal_from_i64(days)? * Decimal::from(86_400))
}

fn time_to_seconds(value: &TimeValue) -> Result<Decimal, XPathError> {
    time_to_seconds_components(value.hour, value.minute, value.second)
}

fn time_to_seconds_components(
    hour: u8,
    minute: u8,
    second: Decimal,
) -> Result<Decimal, XPathError> {
    let hours = decimal_from_i64(hour as i64)?;
    let minutes = decimal_from_i64(minute as i64)?;
    Ok(hours * Decimal::from(3_600) + minutes * Decimal::from(60) + second)
}

fn instant_to_datetime(
    instant: Decimal,
    timezone: Option<TimezoneOffset>,
) -> Result<DateTimeValue, XPathError> {
    let day_seconds = decimal_from_i64(86_400)?;
    let days = (instant / day_seconds).floor();
    let mut seconds_in_day = instant - days * day_seconds;
    if seconds_in_day < Decimal::ZERO {
        seconds_in_day += day_seconds;
    }
    let days = decimal_to_i64(days, "day count")?;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = time_components_from_seconds(seconds_in_day)?;
    Ok(DateTimeValue {
        year,
        month,
        day,
        hour,
        minute,
        second,
        timezone,
    })
}

fn instant_to_date(
    instant: Decimal,
    timezone: Option<TimezoneOffset>,
) -> Result<DateValue, XPathError> {
    let day_seconds = decimal_from_i64(86_400)?;
    let days = (instant / day_seconds).floor();
    let days = decimal_to_i64(days, "day count")?;
    let (year, month, day) = civil_from_days(days);
    Ok(DateValue {
        year,
        month,
        day,
        timezone,
    })
}

fn normalize_seconds_in_day(seconds: Decimal) -> Result<Decimal, XPathError> {
    let day_seconds = decimal_from_i64(86_400)?;
    let days = (seconds / day_seconds).floor();
    let mut remainder = seconds - days * day_seconds;
    if remainder < Decimal::ZERO {
        remainder += day_seconds;
    }
    Ok(remainder)
}

fn time_components_from_seconds(
    seconds: Decimal,
) -> Result<(u8, u8, Decimal), XPathError> {
    let hours = (seconds / Decimal::from(3_600)).floor();
    let mut remainder = seconds - hours * Decimal::from(3_600);
    let minutes = (remainder / Decimal::from(60)).floor();
    remainder -= minutes * Decimal::from(60);
    let hour = decimal_to_u8(hours, "hours")?;
    let minute = decimal_to_u8(minutes, "minutes")?;
    Ok((hour, minute, remainder))
}

fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = year as i64 - if month <= 2 { 1 } else { 0 };
    let m = month as i64;
    let d = day as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m as u8, d as u8)
}

fn decimal_from_i64(value: i64) -> Result<Decimal, XPathError> {
    Decimal::from_i64(value)
        .ok_or_else(|| XPathError::internal("Failed to convert integer to decimal"))
}

fn decimal_to_i64(value: Decimal, label: &str) -> Result<i64, XPathError> {
    value
        .to_i64()
        .ok_or_else(|| XPathError::internal(format!("Failed to convert {} to integer", label)))
}

fn decimal_to_u32(value: Decimal, label: &str) -> Result<u32, XPathError> {
    let val = decimal_to_i64(value, label)?;
    u32::try_from(val)
        .map_err(|_| XPathError::internal(format!("{} out of range", label)))
}

fn decimal_to_u8(value: Decimal, label: &str) -> Result<u8, XPathError> {
    let val = decimal_to_i64(value, label)?;
    u8::try_from(val)
        .map_err(|_| XPathError::internal(format!("{} out of range", label)))
}

fn is_string_like(code: XmlTypeCode) -> bool {
    code.is_string_derived() || matches!(code, XmlTypeCode::AnyUri | XmlTypeCode::UntypedAtomic)
}

// ============================================================================
// General Comparison Support (for sequence comparisons)
// ============================================================================

/// Perform magnitude relationship promotion for general comparisons.
///
/// When comparing values in a general comparison, UntypedAtomic values are
/// promoted to match the type of the other operand according to XPath 2.0 rules:
///
/// - If the other operand is numeric, UntypedAtomic is promoted to xs:double
/// - If the other operand is string, UntypedAtomic is kept as string
/// - If the other operand is a typed value, UntypedAtomic is cast to that type
///
/// # Arguments
///
/// * `left` - The left operand
/// * `right` - The right operand
///
/// # Returns
///
/// A tuple of (promoted_left, promoted_right) suitable for comparison
pub fn magnitude_relationship(
    left: &XmlValue,
    right: &XmlValue,
) -> Result<(XmlValue, XmlValue), XPathError> {
    let mut left_result = left.clone();
    let mut right_result = right.clone();

    // Promote left if it's UntypedAtomic
    if left.type_code == XmlTypeCode::UntypedAtomic {
        if right.type_code.is_numeric() {
            // Promote to double
            let s = left.to_string_value();
            let d: f64 = s.trim().parse().map_err(|_| {
                XPathError::invalid_cast_value(&s, "xs:double")
            })?;
            left_result = XmlValue::double(d);
        } else if is_string_like(right.type_code) {
            // Keep as string
            left_result = XmlValue::string(left.to_string_value());
        }
        // For other types, we'd need full cast support - for now, keep as string
    }

    // Promote right if it's UntypedAtomic
    if right.type_code == XmlTypeCode::UntypedAtomic {
        if left_result.type_code.is_numeric() {
            // Promote to double
            let s = right.to_string_value();
            let d: f64 = s.trim().parse().map_err(|_| {
                XPathError::invalid_cast_value(&s, "xs:double")
            })?;
            right_result = XmlValue::double(d);
        } else if is_string_like(left_result.type_code) {
            // Keep as string
            right_result = XmlValue::string(right.to_string_value());
        }
        // For other types, we'd need full cast support - for now, keep as string
    }

    Ok((left_result, right_result))
}

/// Perform magnitude relationship promotion with context-aware casting.
///
/// This mirrors `MagnitudeRelationship` in C# with support for schema-aware casts.
pub fn magnitude_relationship_ctx(
    _context: &XPathContext,
    left: &XmlValue,
    right: &XmlValue,
) -> Result<(XmlValue, XmlValue), XPathError> {
    let mut left_result = left.clone();
    let mut right_result = right.clone();

    if left_result.type_code == XmlTypeCode::UntypedAtomic {
        if right.type_code.is_numeric() {
            let s = left_result.to_string_value();
            let d: f64 = s.trim().parse().map_err(|_| {
                XPathError::invalid_cast_value(&s, "xs:double")
            })?;
            left_result = XmlValue::double(d);
        } else if is_string_like(right.type_code) {
            left_result = XmlValue::string(left_result.to_string_value());
        } else if right.type_code != XmlTypeCode::UntypedAtomic {
            left_result = cast_to(&left_result, right.type_code)?;
        }
    }

    if right_result.type_code == XmlTypeCode::UntypedAtomic {
        if left_result.type_code.is_numeric() {
            let s = right_result.to_string_value();
            let d: f64 = s.trim().parse().map_err(|_| {
                XPathError::invalid_cast_value(&s, "xs:double")
            })?;
            right_result = XmlValue::double(d);
        } else if is_string_like(left_result.type_code) {
            right_result = XmlValue::string(right_result.to_string_value());
        } else if left_result.type_code != XmlTypeCode::UntypedAtomic {
            right_result = cast_to(&right_result, left_result.type_code)?;
        }
    }

    Ok((left_result, right_result))
}

fn atomize_item<N: DomNavigator>(item: XmlItemRef<'_, N>) -> Result<XmlValue, XPathError> {
    Ok(match item {
        XmlItemRef::Atomic(value) => value.clone(),
        XmlItemRef::Node(node) => node.atomized_value(),
    })
}

/// Compare two values for equality (value comparison).
///
/// This is the core equality comparison used by both value and general comparisons.
/// For general comparisons, use `magnitude_relationship` first to promote UntypedAtomic values.
pub fn value_eq(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    compare_eq(left, right)
}

/// Compare two values for greater-than (value comparison).
pub fn value_gt(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    compare_gt(left, right)
}

/// Compare two values for greater-than-or-equal (value comparison).
pub fn value_ge(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    compare_ge(left, right)
}

/// Compare two values for less-than (value comparison).
pub fn value_lt(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    compare_lt(left, right)
}

/// Compare two values for less-than-or-equal (value comparison).
pub fn value_le(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    compare_le(left, right)
}

// ============================================================================
// General comparisons for single atomized values
// ============================================================================

/// General equality comparison with magnitude relationship promotion (single values).
///
/// Promotes UntypedAtomic values before comparison.
/// For sequence comparisons, use `general_eq_seq`.
pub fn general_eq(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let (l, r) = magnitude_relationship(left, right)?;
    compare_eq(&l, &r)
}

/// General greater-than comparison with magnitude relationship promotion (single values).
pub fn general_gt(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let (l, r) = magnitude_relationship(left, right)?;
    compare_gt(&l, &r)
}

/// General not-equal comparison with magnitude relationship promotion (single values).
pub fn general_ne(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    general_eq(left, right).map(|eq| !eq)
}

/// General greater-than-or-equal comparison with magnitude relationship promotion (single values).
pub fn general_ge(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let (l, r) = magnitude_relationship(left, right)?;
    compare_ge(&l, &r)
}

/// General less-than comparison with magnitude relationship promotion (single values).
pub fn general_lt(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let (l, r) = magnitude_relationship(left, right)?;
    compare_lt(&l, &r)
}

/// General less-than-or-equal comparison with magnitude relationship promotion (single values).
pub fn general_le(left: &XmlValue, right: &XmlValue) -> Result<bool, XPathError> {
    let (l, r) = magnitude_relationship(left, right)?;
    compare_le(&l, &r)
}

// ============================================================================
// Iterator-based general comparisons (Cartesian product semantics)
// ============================================================================

pub fn general_eq_iter<I1, I2>(
    context: &XPathContext,
    left: &I1,
    right: &I2,
) -> Result<bool, XPathError>
where
    I1: XmlNodeIterator,
    I2: XmlNodeIterator,
{
    let right_buf = BufferedNodeIterator::preload(right.clone())?;
    let mut left_iter = left.clone();

    while left_iter.move_next()? {
        let left_item = left_iter
            .current()
            .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
        let left_value = atomize_item(left_item)?;
        let mut right_iter = right_buf.clone();

        while right_iter.move_next()? {
            let right_item = right_iter
                .current()
                .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
            let right_value = atomize_item(right_item)?;
            let (l, r) = magnitude_relationship_ctx(context, &left_value, &right_value)?;

            match value_eq(&l, &r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(err) if is_operator_not_defined(&err) => continue,
                Err(err) => return Err(err),
            }
        }
    }

    Ok(false)
}

pub fn general_ne_iter<I1, I2>(
    context: &XPathContext,
    left: &I1,
    right: &I2,
) -> Result<bool, XPathError>
where
    I1: XmlNodeIterator,
    I2: XmlNodeIterator,
{
    let right_buf = BufferedNodeIterator::preload(right.clone())?;
    let mut left_iter = left.clone();

    while left_iter.move_next()? {
        let left_item = left_iter
            .current()
            .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
        let left_value = atomize_item(left_item)?;
        let mut right_iter = right_buf.clone();

        while right_iter.move_next()? {
            let right_item = right_iter
                .current()
                .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
            let right_value = atomize_item(right_item)?;
            let (l, r) = magnitude_relationship_ctx(context, &left_value, &right_value)?;

            match value_eq(&l, &r) {
                Ok(true) => continue,
                Ok(false) => return Ok(true),
                Err(err) if is_operator_not_defined(&err) => return Ok(true),
                Err(err) => return Err(err),
            }
        }
    }

    Ok(false)
}

pub fn general_lt_iter<I1, I2>(
    context: &XPathContext,
    left: &I1,
    right: &I2,
) -> Result<bool, XPathError>
where
    I1: XmlNodeIterator,
    I2: XmlNodeIterator,
{
    let right_buf = BufferedNodeIterator::preload(right.clone())?;
    let mut left_iter = left.clone();

    while left_iter.move_next()? {
        let left_item = left_iter
            .current()
            .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
        let left_value = atomize_item(left_item)?;
        let mut right_iter = right_buf.clone();

        while right_iter.move_next()? {
            let right_item = right_iter
                .current()
                .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
            let right_value = atomize_item(right_item)?;
            let (l, r) = magnitude_relationship_ctx(context, &left_value, &right_value)?;

            match value_lt(&l, &r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(err) => return Err(err),
            }
        }
    }

    Ok(false)
}

pub fn general_le_iter<I1, I2>(
    context: &XPathContext,
    left: &I1,
    right: &I2,
) -> Result<bool, XPathError>
where
    I1: XmlNodeIterator,
    I2: XmlNodeIterator,
{
    let right_buf = BufferedNodeIterator::preload(right.clone())?;
    let mut left_iter = left.clone();

    while left_iter.move_next()? {
        let left_item = left_iter
            .current()
            .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
        let left_value = atomize_item(left_item)?;
        let mut right_iter = right_buf.clone();

        while right_iter.move_next()? {
            let right_item = right_iter
                .current()
                .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
            let right_value = atomize_item(right_item)?;
            let (l, r) = magnitude_relationship_ctx(context, &left_value, &right_value)?;

            match value_eq(&l, &r) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(err) if is_operator_not_defined(&err) => {}
                Err(err) => return Err(err),
            }

            match value_lt(&l, &r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(err) => return Err(err),
            }
        }
    }

    Ok(false)
}

pub fn general_gt_iter<I1, I2>(
    context: &XPathContext,
    left: &I1,
    right: &I2,
) -> Result<bool, XPathError>
where
    I1: XmlNodeIterator,
    I2: XmlNodeIterator,
{
    let right_buf = BufferedNodeIterator::preload(right.clone())?;
    let mut left_iter = left.clone();

    while left_iter.move_next()? {
        let left_item = left_iter
            .current()
            .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
        let left_value = atomize_item(left_item)?;
        let mut right_iter = right_buf.clone();

        while right_iter.move_next()? {
            let right_item = right_iter
                .current()
                .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
            let right_value = atomize_item(right_item)?;
            let (l, r) = magnitude_relationship_ctx(context, &left_value, &right_value)?;

            match value_gt(&l, &r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(err) => return Err(err),
            }
        }
    }

    Ok(false)
}

pub fn general_ge_iter<I1, I2>(
    context: &XPathContext,
    left: &I1,
    right: &I2,
) -> Result<bool, XPathError>
where
    I1: XmlNodeIterator,
    I2: XmlNodeIterator,
{
    let right_buf = BufferedNodeIterator::preload(right.clone())?;
    let mut left_iter = left.clone();

    while left_iter.move_next()? {
        let left_item = left_iter
            .current()
            .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
        let left_value = atomize_item(left_item)?;
        let mut right_iter = right_buf.clone();

        while right_iter.move_next()? {
            let right_item = right_iter
                .current()
                .ok_or_else(|| XPathError::internal("Iterator current missing"))?;
            let right_value = atomize_item(right_item)?;
            let (l, r) = magnitude_relationship_ctx(context, &left_value, &right_value)?;

            match value_eq(&l, &r) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(err) if is_operator_not_defined(&err) => {}
                Err(err) => return Err(err),
            }

            match value_gt(&l, &r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(err) => return Err(err),
            }
        }
    }

    Ok(false)
}

// ============================================================================
// General comparisons for sequences (Cartesian product semantics)
// ============================================================================

/// General equality comparison for sequences (Cartesian product).
///
/// Returns true if ANY pair (left_item, right_item) from the Cartesian product
/// of the two sequences satisfies the equality condition.
///
/// # XPath 2.0 Semantics
///
/// The general comparison operators (`=`, `!=`, `<`, `<=`, `>`, `>=`) are
/// existentially quantified over their operand sequences:
/// - `A = B` is true if there exist atomized values `a` in `A` and `b` in `B`
///   such that `a eq b` is true (after type promotion).
///
/// # Arguments
///
/// * `left` - Left sequence of atomized values
/// * `right` - Right sequence of atomized values
///
/// # Returns
///
/// `true` if any pair satisfies equality, `false` if no pairs satisfy or
/// either sequence is empty.
pub fn general_eq_seq(left: &[XmlValue], right: &[XmlValue]) -> Result<bool, XPathError> {
    // Empty sequences: result is false (no pairs exist)
    if left.is_empty() || right.is_empty() {
        return Ok(false);
    }

    // Cartesian product: check if ANY pair satisfies the condition
    for l in left {
        for r in right {
            match general_eq(l, r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(_) => continue, // Type errors mean this pair doesn't match
            }
        }
    }

    Ok(false)
}

/// General not-equal comparison for sequences (Cartesian product).
///
/// Returns true if ANY pair (left_item, right_item) satisfies inequality.
pub fn general_ne_seq(left: &[XmlValue], right: &[XmlValue]) -> Result<bool, XPathError> {
    if left.is_empty() || right.is_empty() {
        return Ok(false);
    }

    for l in left {
        for r in right {
            match general_ne(l, r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(_) => continue,
            }
        }
    }

    Ok(false)
}

/// General less-than comparison for sequences (Cartesian product).
///
/// Returns true if ANY pair (left_item, right_item) satisfies left < right.
pub fn general_lt_seq(left: &[XmlValue], right: &[XmlValue]) -> Result<bool, XPathError> {
    if left.is_empty() || right.is_empty() {
        return Ok(false);
    }

    for l in left {
        for r in right {
            match general_lt(l, r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(_) => continue,
            }
        }
    }

    Ok(false)
}

/// General less-than-or-equal comparison for sequences (Cartesian product).
///
/// Returns true if ANY pair (left_item, right_item) satisfies left <= right.
pub fn general_le_seq(left: &[XmlValue], right: &[XmlValue]) -> Result<bool, XPathError> {
    if left.is_empty() || right.is_empty() {
        return Ok(false);
    }

    for l in left {
        for r in right {
            match general_le(l, r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(_) => continue,
            }
        }
    }

    Ok(false)
}

/// General greater-than comparison for sequences (Cartesian product).
///
/// Returns true if ANY pair (left_item, right_item) satisfies left > right.
pub fn general_gt_seq(left: &[XmlValue], right: &[XmlValue]) -> Result<bool, XPathError> {
    if left.is_empty() || right.is_empty() {
        return Ok(false);
    }

    for l in left {
        for r in right {
            match general_gt(l, r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(_) => continue,
            }
        }
    }

    Ok(false)
}

/// General greater-than-or-equal comparison for sequences (Cartesian product).
///
/// Returns true if ANY pair (left_item, right_item) satisfies left >= right.
pub fn general_ge_seq(left: &[XmlValue], right: &[XmlValue]) -> Result<bool, XPathError> {
    if left.is_empty() || right.is_empty() {
        return Ok(false);
    }

    for l in left {
        for r in right {
            match general_ge(l, r) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(_) => continue,
            }
        }
    }

    Ok(false)
}

fn unsupported_operator(op: BinaryOpKind, left: &XmlValue, right: &XmlValue) -> XPathError {
    XPathError::internal(format!(
        "Operator {:?} not defined for types {:?} and {:?}",
        op, left.type_code, right.type_code
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::NameTable;
    use crate::namespace::qname::QualifiedName;
    use crate::xpath::context::XPathContext;
    use crate::xpath::iterator::{VecNodeIterator, XmlItem};
    use crate::xpath::roxmltree::RoXmlNavigator;

    fn int_value(type_code: XmlTypeCode, value: i64) -> XmlValue {
        XmlValue {
            type_code,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Integer(BigInt::from(value))),
        }
    }

    fn decimal_value(value: &str) -> XmlValue {
        XmlValue {
            type_code: XmlTypeCode::Decimal,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Decimal(
                value.parse::<Decimal>().unwrap(),
            )),
        }
    }

    fn datetime_value(
        type_code: XmlTypeCode,
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: Decimal,
    ) -> XmlValue {
        XmlValue {
            type_code,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::DateTime(DateTimeValue {
                year,
                month,
                day,
                hour,
                minute,
                second,
                timezone: None,
            })),
        }
    }

    fn date_value(year: i32, month: u8, day: u8) -> XmlValue {
        XmlValue {
            type_code: XmlTypeCode::Date,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Date(DateValue {
                year,
                month,
                day,
                timezone: None,
            })),
        }
    }

    fn time_value(hour: u8, minute: u8, second: Decimal) -> XmlValue {
        XmlValue {
            type_code: XmlTypeCode::Time,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Time(TimeValue {
                hour,
                minute,
                second,
                timezone: None,
            })),
        }
    }

    fn time_value_with_tz(
        hour: u8,
        minute: u8,
        second: Decimal,
        timezone: TimezoneOffset,
    ) -> XmlValue {
        XmlValue {
            type_code: XmlTypeCode::Time,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Time(TimeValue {
                hour,
                minute,
                second,
                timezone: Some(timezone),
            })),
        }
    }

    fn year_month_duration_value(years: u32, months: u32) -> XmlValue {
        XmlValue {
            type_code: XmlTypeCode::YearMonthDuration,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(
                YearMonthDurationValue {
                    negative: false,
                    years,
                    months,
                },
            )),
        }
    }

    fn day_time_duration_value(days: u32, hours: u32, minutes: u32, seconds: Decimal) -> XmlValue {
        XmlValue {
            type_code: XmlTypeCode::DayTimeDuration,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(
                DayTimeDurationValue {
                    negative: false,
                    days,
                    hours,
                    minutes,
                    seconds,
                },
            )),
        }
    }

    fn duration_value(
        negative: bool,
        years: u32,
        months: u32,
        days: u32,
        hours: u32,
        minutes: u32,
        seconds: Decimal,
    ) -> XmlValue {
        XmlValue {
            type_code: XmlTypeCode::Duration,
            schema_type: None,
            value: XmlValueKind::Atomic(XmlAtomicValue::Duration(DurationValue {
                negative,
                years,
                months,
                days,
                hours,
                minutes,
                seconds,
            })),
        }
    }

    #[test]
    fn test_add_byte_unsigned_byte_returns_int() {
        let left = int_value(XmlTypeCode::Byte, 1);
        let right = int_value(XmlTypeCode::UnsignedByte, 2);
        let result = eval_binary(BinaryOpKind::Add, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Int);
        assert_eq!(result.as_integer().unwrap(), &BigInt::from(3));
    }

    #[test]
    fn test_add_int_unsigned_int_returns_unsigned_int() {
        let left = int_value(XmlTypeCode::Int, 3);
        let right = int_value(XmlTypeCode::UnsignedInt, 4);
        let result = eval_binary(BinaryOpKind::Add, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::UnsignedInt);
        assert_eq!(result.as_integer().unwrap(), &BigInt::from(7));
    }

    #[test]
    fn test_div_int_int_returns_decimal() {
        let left = int_value(XmlTypeCode::Int, 3);
        let right = int_value(XmlTypeCode::Int, 2);
        let result = eval_binary(BinaryOpKind::Div, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Decimal);
        assert_eq!(result.as_decimal().unwrap(), Decimal::new(15, 1));
    }

    #[test]
    fn test_idiv_double_truncates() {
        let left = XmlValue::double(5.9);
        let right = XmlValue::double(2.0);
        let result = eval_binary(BinaryOpKind::IDiv, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Integer);
        assert_eq!(result.as_integer().unwrap(), &BigInt::from(2));
    }

    #[test]
    fn test_unary_minus_unsigned_int_returns_long() {
        let value = int_value(XmlTypeCode::UnsignedInt, 5);
        let result = eval_unary(UnaryOpKind::Negate, &value).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Long);
        assert_eq!(result.as_integer().unwrap(), &BigInt::from(-5));
    }

    #[test]
    fn test_string_comparison() {
        let left = XmlValue::string("alpha");
        let right = XmlValue::string("beta");
        let result = eval_binary(BinaryOpKind::GeneralLt, &left, &right).unwrap();
        assert_eq!(result.as_boolean(), Some(true));
    }

    #[test]
    fn test_boolean_eq() {
        let left = XmlValue::boolean(true);
        let right = XmlValue::boolean(false);
        let result = eval_binary(BinaryOpKind::GeneralEq, &left, &right).unwrap();
        assert_eq!(result.as_boolean(), Some(false));
    }

    #[test]
    fn test_range_integer_sequence() {
        let start = int_value(XmlTypeCode::Integer, 1);
        let end = int_value(XmlTypeCode::Integer, 3);
        let result = eval_range(&start, &end).unwrap();
        let values: Vec<_> = result
            .iter()
            .map(|v| v.as_integer().unwrap().clone())
            .collect();
        assert_eq!(values, vec![BigInt::from(1), BigInt::from(2), BigInt::from(3)]);
    }

    #[test]
    fn test_decimal_eq() {
        let left = decimal_value("2.5");
        let right = decimal_value("2.5");
        let result = eval_binary(BinaryOpKind::GeneralEq, &left, &right).unwrap();
        assert_eq!(result.as_boolean(), Some(true));
    }

    #[test]
    fn test_datetime_add_year_month_clamps_day() {
        let left = datetime_value(
            XmlTypeCode::DateTime,
            2023,
            1,
            31,
            10,
            0,
            Decimal::ZERO,
        );
        let right = year_month_duration_value(0, 1);
        let result = eval_binary(BinaryOpKind::Add, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::DateTime);
        match result.value {
            XmlValueKind::Atomic(XmlAtomicValue::DateTime(dt)) => {
                assert_eq!((dt.year, dt.month, dt.day), (2023, 2, 28));
                assert_eq!((dt.hour, dt.minute), (10, 0));
            }
            _ => panic!("Expected dateTime result"),
        }
    }

    #[test]
    fn test_date_sub_date_returns_day_time_duration() {
        let left = date_value(2024, 3, 15);
        let right = date_value(2024, 3, 14);
        let result = eval_binary(BinaryOpKind::Sub, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::DayTimeDuration);
        match result.value {
            XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(duration)) => {
                assert!(!duration.negative);
                assert_eq!(duration.days, 1);
                assert_eq!(duration.hours, 0);
                assert_eq!(duration.minutes, 0);
                assert!(duration.seconds.is_zero());
            }
            _ => panic!("Expected dayTimeDuration result"),
        }
    }

    #[test]
    fn test_time_add_day_time_wraps() {
        let left = time_value(23, 0, Decimal::ZERO);
        let right = day_time_duration_value(0, 2, 0, Decimal::ZERO);
        let result = eval_binary(BinaryOpKind::Add, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Time);
        match result.value {
            XmlValueKind::Atomic(XmlAtomicValue::Time(time)) => {
                assert_eq!((time.hour, time.minute), (1, 0));
            }
            _ => panic!("Expected time result"),
        }
    }

    #[test]
    fn test_time_compare_uses_implicit_timezone() {
        let implicit = implicit_timezone_offset();
        let left = time_value(10, 0, Decimal::ZERO);
        let right = time_value_with_tz(10, 0, Decimal::ZERO, implicit);
        let result = eval_binary(BinaryOpKind::GeneralEq, &left, &right).unwrap();
        assert_eq!(result.as_boolean(), Some(true));
    }

    #[test]
    fn test_numeric_mul_year_month_duration() {
        let left = int_value(XmlTypeCode::Int, 2);
        let right = year_month_duration_value(1, 2);
        let result = eval_binary(BinaryOpKind::Mul, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::YearMonthDuration);
        match result.value {
            XmlValueKind::Atomic(XmlAtomicValue::YearMonthDuration(duration)) => {
                assert_eq!((duration.years, duration.months), (2, 4));
            }
            _ => panic!("Expected yearMonthDuration result"),
        }
    }

    #[test]
    fn test_day_time_duration_div_duration_returns_decimal() {
        let left = day_time_duration_value(0, 3, 0, Decimal::ZERO);
        let right = day_time_duration_value(0, 1, 0, Decimal::ZERO);
        let result = eval_binary(BinaryOpKind::Div, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::Decimal);
        assert_eq!(result.as_decimal(), Some(Decimal::from(3)));
    }

    #[test]
    fn test_duration_eq_across_subtypes() {
        let left = duration_value(false, 1, 2, 0, 0, 0, Decimal::ZERO);
        let right = year_month_duration_value(1, 2);
        let result = eval_binary(BinaryOpKind::GeneralEq, &left, &right).unwrap();
        assert_eq!(result.as_boolean(), Some(true));
    }

    #[test]
    fn test_datetime_sub_datetime_returns_day_time_duration() {
        let left = datetime_value(
            XmlTypeCode::DateTime,
            2024,
            3,
            15,
            12,
            0,
            Decimal::ZERO,
        );
        let right = datetime_value(
            XmlTypeCode::DateTime,
            2024,
            3,
            15,
            11,
            0,
            Decimal::ZERO,
        );
        let result = eval_binary(BinaryOpKind::Sub, &left, &right).unwrap();
        assert_eq!(result.type_code, XmlTypeCode::DayTimeDuration);
        match result.value {
            XmlValueKind::Atomic(XmlAtomicValue::DayTimeDuration(duration)) => {
                assert!(!duration.negative);
                assert_eq!((duration.days, duration.hours, duration.minutes), (0, 1, 0));
            }
            _ => panic!("Expected dayTimeDuration result"),
        }
    }

    // ========================================================================
    // General Comparison Tests
    // ========================================================================

    #[test]
    fn test_magnitude_relationship_untyped_to_numeric() {
        // UntypedAtomic compared with integer should promote to double
        let left = XmlValue::untyped("42");
        let right = int_value(XmlTypeCode::Integer, 42);
        let (promoted_left, promoted_right) = magnitude_relationship(&left, &right).unwrap();
        assert_eq!(promoted_left.type_code, XmlTypeCode::Double);
        assert_eq!(promoted_right.type_code, XmlTypeCode::Integer);
    }

    #[test]
    fn test_magnitude_relationship_untyped_to_string() {
        // UntypedAtomic compared with string should become string
        let left = XmlValue::untyped("hello");
        let right = XmlValue::string("world");
        let (promoted_left, promoted_right) = magnitude_relationship(&left, &right).unwrap();
        assert_eq!(promoted_left.type_code, XmlTypeCode::String);
        assert_eq!(promoted_right.type_code, XmlTypeCode::String);
    }

    #[test]
    fn test_magnitude_relationship_both_untyped() {
        // Both UntypedAtomic should both become string
        let left = XmlValue::untyped("abc");
        let right = XmlValue::untyped("def");
        let (promoted_left, promoted_right) = magnitude_relationship(&left, &right).unwrap();
        // When both are untyped, they stay as string-like
        assert!(is_string_like(promoted_left.type_code));
        assert!(is_string_like(promoted_right.type_code));
    }

    #[test]
    fn test_general_eq_with_untyped() {
        // "42" = 42 should be true (UntypedAtomic promoted to double)
        let left = XmlValue::untyped("42");
        let right = int_value(XmlTypeCode::Integer, 42);
        assert!(general_eq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_eq_strings() {
        let left = XmlValue::string("hello");
        let right = XmlValue::string("hello");
        assert!(general_eq(&left, &right).unwrap());

        let right2 = XmlValue::string("world");
        assert!(!general_eq(&left, &right2).unwrap());
    }

    #[test]
    fn test_general_gt_with_untyped() {
        // "50" > 42 should be true
        let left = XmlValue::untyped("50");
        let right = int_value(XmlTypeCode::Integer, 42);
        assert!(general_gt(&left, &right).unwrap());
    }

    #[test]
    fn test_general_ne() {
        let left = XmlValue::string("a");
        let right = XmlValue::string("b");
        assert!(general_ne(&left, &right).unwrap());

        let same = XmlValue::string("a");
        assert!(!general_ne(&left, &same).unwrap());
    }

    #[test]
    fn test_general_comparisons_numeric() {
        let five = int_value(XmlTypeCode::Integer, 5);
        let ten = int_value(XmlTypeCode::Integer, 10);

        assert!(general_lt(&five, &ten).unwrap());
        assert!(general_le(&five, &ten).unwrap());
        assert!(!general_gt(&five, &ten).unwrap());
        assert!(!general_ge(&five, &ten).unwrap());

        assert!(general_ge(&five, &five).unwrap());
        assert!(general_le(&five, &five).unwrap());
    }

    #[test]
    fn test_value_comparisons() {
        let a = XmlValue::string("abc");
        let b = XmlValue::string("xyz");

        assert!(value_lt(&a, &b).unwrap());
        assert!(value_le(&a, &b).unwrap());
        assert!(!value_gt(&a, &b).unwrap());
        assert!(!value_ge(&a, &b).unwrap());
        assert!(!value_eq(&a, &b).unwrap());
    }

    // ========================================================================
    // Sequence General Comparison Tests (Cartesian product)
    // ========================================================================

    #[test]
    fn test_general_eq_seq_finds_match() {
        // (1, 2, 3) = (3, 4, 5) should be true because 3 appears in both
        let left = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 2),
            int_value(XmlTypeCode::Integer, 3),
        ];
        let right = vec![
            int_value(XmlTypeCode::Integer, 3),
            int_value(XmlTypeCode::Integer, 4),
            int_value(XmlTypeCode::Integer, 5),
        ];
        assert!(general_eq_seq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_eq_seq_no_match() {
        // (1, 2) = (3, 4) should be false because no common values
        let left = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 2),
        ];
        let right = vec![
            int_value(XmlTypeCode::Integer, 3),
            int_value(XmlTypeCode::Integer, 4),
        ];
        assert!(!general_eq_seq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_eq_seq_empty_is_false() {
        // Empty sequences always return false for general comparisons
        let left: Vec<XmlValue> = vec![];
        let right = vec![int_value(XmlTypeCode::Integer, 1)];
        assert!(!general_eq_seq(&left, &right).unwrap());
        assert!(!general_eq_seq(&right, &left).unwrap());
        assert!(!general_eq_seq(&left, &left).unwrap());
    }

    #[test]
    fn test_general_ne_seq() {
        // (1, 2) != (2, 3) should be true because 1 != 2, 1 != 3, 2 != 3 all true
        let left = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 2),
        ];
        let right = vec![
            int_value(XmlTypeCode::Integer, 2),
            int_value(XmlTypeCode::Integer, 3),
        ];
        assert!(general_ne_seq(&left, &right).unwrap());

        // (1) != (1) should be false because no pair is not-equal
        let same = vec![int_value(XmlTypeCode::Integer, 1)];
        assert!(!general_ne_seq(&same, &same).unwrap());
    }

    #[test]
    fn test_general_lt_seq() {
        // (1, 2) < (3, 4) should be true because 1 < 3, 1 < 4, 2 < 3, 2 < 4
        let left = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 2),
        ];
        let right = vec![
            int_value(XmlTypeCode::Integer, 3),
            int_value(XmlTypeCode::Integer, 4),
        ];
        assert!(general_lt_seq(&left, &right).unwrap());

        // (3, 4) < (1, 2) should be false
        assert!(!general_lt_seq(&right, &left).unwrap());

        // (1, 5) < (3, 4) should be true because 1 < 3, 1 < 4
        let mixed = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 5),
        ];
        assert!(general_lt_seq(&mixed, &right).unwrap());
    }

    #[test]
    fn test_general_gt_seq() {
        // (3, 4) > (1, 2) should be true
        let left = vec![
            int_value(XmlTypeCode::Integer, 3),
            int_value(XmlTypeCode::Integer, 4),
        ];
        let right = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 2),
        ];
        assert!(general_gt_seq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_le_seq() {
        // (1, 2) <= (2, 3) should be true because 1 <= 2, 1 <= 3, 2 <= 2, 2 <= 3
        let left = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 2),
        ];
        let right = vec![
            int_value(XmlTypeCode::Integer, 2),
            int_value(XmlTypeCode::Integer, 3),
        ];
        assert!(general_le_seq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_ge_seq() {
        // (2, 3) >= (1, 2) should be true
        let left = vec![
            int_value(XmlTypeCode::Integer, 2),
            int_value(XmlTypeCode::Integer, 3),
        ];
        let right = vec![
            int_value(XmlTypeCode::Integer, 1),
            int_value(XmlTypeCode::Integer, 2),
        ];
        assert!(general_ge_seq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_seq_with_type_promotion() {
        // UntypedAtomic should be promoted: ("42") = (42) should be true
        let left = vec![XmlValue::untyped("42")];
        let right = vec![int_value(XmlTypeCode::Integer, 42)];
        assert!(general_eq_seq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_seq_mixed_types() {
        // Mixed types that can't compare just skip those pairs
        // (1, "hello") = ("hello", 2) should be true because "hello" = "hello"
        let left = vec![
            int_value(XmlTypeCode::Integer, 1),
            XmlValue::string("hello"),
        ];
        let right = vec![
            XmlValue::string("hello"),
            int_value(XmlTypeCode::Integer, 2),
        ];
        assert!(general_eq_seq(&left, &right).unwrap());
    }

    #[test]
    fn test_compare_ge_prefers_eq_over_ordering() {
        // QName ordering isn't defined, but equality is.
        let names = NameTable::new();
        let local = names.add("a");
        let qname = QualifiedName::local(local);
        let left = XmlValue::new(
            XmlTypeCode::QName,
            XmlValueKind::Atomic(XmlAtomicValue::QName(qname)),
        );
        let right = left.clone();

        assert!(compare_ge(&left, &right).unwrap());
        assert!(compare_le(&left, &right).unwrap());
    }

    #[test]
    fn test_list_equality() {
        let left = XmlValue::new(
            XmlTypeCode::NmTokens,
            XmlValueKind::List {
                item_type: XmlTypeCode::NmToken,
                items: vec![
                    XmlAtomicValue::String("a".to_string()),
                    XmlAtomicValue::String("b".to_string()),
                ],
            },
        );
        let right = left.clone();
        let different = XmlValue::new(
            XmlTypeCode::NmTokens,
            XmlValueKind::List {
                item_type: XmlTypeCode::NmToken,
                items: vec![XmlAtomicValue::String("a".to_string())],
            },
        );

        assert!(compare_eq(&left, &right).unwrap());
        assert!(!compare_eq(&left, &different).unwrap());
    }

    #[test]
    fn test_union_unwrap_equality() {
        let inner = XmlValue::string("hello");
        let left = XmlValue::new(XmlTypeCode::String, XmlValueKind::Union(Box::new(inner)));
        let right = XmlValue::string("hello");
        assert!(compare_eq(&left, &right).unwrap());
    }

    #[test]
    fn test_general_eq_iter_finds_match() {
        let names = NameTable::new();
        let context = XPathContext::new(&names);
        let left: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
        ]);
        let right: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
        ]);

        assert!(general_eq_iter(&context, &left, &right).unwrap());
    }

    #[test]
    fn test_general_eq_iter_invalid_cast_errors() {
        let names = NameTable::new();
        let context = XPathContext::new(&names);
        let left: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::untyped("not-a-number")),
        ]);
        let right: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
        ]);

        let result = general_eq_iter(&context, &left, &right);
        assert!(matches!(result, Err(XPathError::FORG0001 { .. })));
    }

    #[test]
    fn test_general_eq_iter_type_mismatch_is_false() {
        let names = NameTable::new();
        let context = XPathContext::new(&names);
        let left: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::boolean(true)),
        ]);
        let right: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(date_value(2024, 1, 1)),
        ]);

        assert!(!general_eq_iter(&context, &left, &right).unwrap());
    }

    #[test]
    fn test_general_gt_iter_type_mismatch_errors() {
        let names = NameTable::new();
        let context = XPathContext::new(&names);
        let left: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::boolean(true)),
        ]);
        let right: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::string("false")),
        ]);

        let result = general_gt_iter(&context, &left, &right);
        assert!(matches!(result, Err(XPathError::BinaryOperatorNotDefined { .. })));
    }
}
