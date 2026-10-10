//! Argument names for every function, for the signature hint shown while a
//! formula is typed (`SUMIF(range, criteria, [sum_range])`). Optional
//! arguments are bracketed; `…` repeats the one before it.

const SIGS: &[(&str, &str)] = &[
    // Evaluator forms
    ("IF", "logical_test, value_if_true, [value_if_false]"),
    ("IFS", "condition1, value1, [condition2, value2], …"),
    ("IFERROR", "value, value_if_error"),
    ("IFNA", "value, value_if_na"),
    ("CHOOSE", "index, value1, [value2], …"),
    (
        "SWITCH",
        "expression, value1, result1, [value2, result2], …, [default]",
    ),
    // Math
    ("SUM", "number1, [number2], …"),
    ("PRODUCT", "number1, [number2], …"),
    ("ABS", "number"),
    ("ROUND", "number, num_digits"),
    ("ROUNDUP", "number, num_digits"),
    ("ROUNDDOWN", "number, num_digits"),
    ("INT", "number"),
    ("TRUNC", "number, [num_digits]"),
    ("MOD", "number, divisor"),
    ("POWER", "number, power"),
    ("SQRT", "number"),
    ("EXP", "number"),
    ("LN", "number"),
    ("LOG", "number, [base]"),
    ("LOG10", "number"),
    ("CEILING", "number, significance"),
    ("CEILING.MATH", "number, [significance], [mode]"),
    ("FLOOR", "number, significance"),
    ("FLOOR.MATH", "number, [significance], [mode]"),
    ("SIGN", "number"),
    ("PI", ""),
    ("RAND", ""),
    ("RANDBETWEEN", "bottom, top"),
    ("SUMPRODUCT", "array1, [array2], …"),
    ("QUOTIENT", "numerator, denominator"),
    ("GCD", "number1, [number2], …"),
    ("LCM", "number1, [number2], …"),
    ("FACT", "number"),
    ("EVEN", "number"),
    ("ODD", "number"),
    ("MROUND", "number, multiple"),
    // Statistics
    ("AVERAGE", "number1, [number2], …"),
    ("AVERAGEA", "value1, [value2], …"),
    ("COUNT", "value1, [value2], …"),
    ("COUNTA", "value1, [value2], …"),
    ("COUNTBLANK", "range"),
    ("MIN", "number1, [number2], …"),
    ("MAX", "number1, [number2], …"),
    ("MINA", "value1, [value2], …"),
    ("MAXA", "value1, [value2], …"),
    ("MEDIAN", "number1, [number2], …"),
    ("MODE", "number1, [number2], …"),
    ("MODE.SNGL", "number1, [number2], …"),
    ("STDEV", "number1, [number2], …"),
    ("STDEV.S", "number1, [number2], …"),
    ("STDEV.P", "number1, [number2], …"),
    ("STDEVP", "number1, [number2], …"),
    ("VAR", "number1, [number2], …"),
    ("VAR.S", "number1, [number2], …"),
    ("VAR.P", "number1, [number2], …"),
    ("VARP", "number1, [number2], …"),
    ("LARGE", "array, k"),
    ("SMALL", "array, k"),
    ("RANK", "number, ref, [order]"),
    ("RANK.EQ", "number, ref, [order]"),
    ("RANK.AVG", "number, ref, [order]"),
    ("PERCENTILE", "array, k"),
    ("PERCENTILE.INC", "array, k"),
    ("QUARTILE", "array, quart"),
    ("QUARTILE.INC", "array, quart"),
    ("SUMIF", "range, criteria, [sum_range]"),
    (
        "SUMIFS",
        "sum_range, criteria_range1, criteria1, [criteria_range2, criteria2], …",
    ),
    ("COUNTIF", "range, criteria"),
    (
        "COUNTIFS",
        "criteria_range1, criteria1, [criteria_range2, criteria2], …",
    ),
    ("AVERAGEIF", "range, criteria, [average_range]"),
    (
        "AVERAGEIFS",
        "average_range, criteria_range1, criteria1, [criteria_range2, criteria2], …",
    ),
    (
        "MAXIFS",
        "max_range, criteria_range1, criteria1, [criteria_range2, criteria2], …",
    ),
    (
        "MINIFS",
        "min_range, criteria_range1, criteria1, [criteria_range2, criteria2], …",
    ),
    // Logic
    ("AND", "logical1, [logical2], …"),
    ("OR", "logical1, [logical2], …"),
    ("XOR", "logical1, [logical2], …"),
    ("NOT", "logical"),
    ("TRUE", ""),
    ("FALSE", ""),
    // Lookup
    (
        "VLOOKUP",
        "lookup_value, table_array, col_index_num, [range_lookup]",
    ),
    (
        "HLOOKUP",
        "lookup_value, table_array, row_index_num, [range_lookup]",
    ),
    (
        "XLOOKUP",
        "lookup_value, lookup_array, return_array, [if_not_found], [match_mode], [search_mode]",
    ),
    ("MATCH", "lookup_value, lookup_array, [match_type]"),
    ("INDEX", "array, row_num, [column_num], [area_num]"),
    ("ROW", "[reference]"),
    ("COLUMN", "[reference]"),
    ("ROWS", "array"),
    ("COLUMNS", "array"),
    // Text
    ("LEN", "text"),
    ("LEFT", "text, [num_chars]"),
    ("RIGHT", "text, [num_chars]"),
    ("MID", "text, start_num, num_chars"),
    ("UPPER", "text"),
    ("LOWER", "text"),
    ("PROPER", "text"),
    ("TRIM", "text"),
    ("CONCAT", "text1, [text2], …"),
    ("CONCATENATE", "text1, [text2], …"),
    ("TEXTJOIN", "delimiter, ignore_empty, text1, [text2], …"),
    ("SUBSTITUTE", "text, old_text, new_text, [instance_num]"),
    ("REPLACE", "old_text, start_num, num_chars, new_text"),
    ("FIND", "find_text, within_text, [start_num]"),
    ("SEARCH", "find_text, within_text, [start_num]"),
    ("VALUE", "text"),
    ("TEXT", "value, format_text"),
    ("REPT", "text, number_times"),
    ("EXACT", "text1, text2"),
    ("CHAR", "number"),
    ("CODE", "text"),
    ("CLEAN", "text"),
    (
        "NUMBERVALUE",
        "text, [decimal_separator], [group_separator]",
    ),
    ("FIXED", "number, [decimals], [no_commas]"),
    ("DOLLAR", "number, [decimals]"),
    // Dates
    ("DATE", "year, month, day"),
    ("TODAY", ""),
    ("NOW", ""),
    ("YEAR", "serial_number"),
    ("MONTH", "serial_number"),
    ("DAY", "serial_number"),
    ("HOUR", "serial_number"),
    ("MINUTE", "serial_number"),
    ("SECOND", "serial_number"),
    ("TIME", "hour, minute, second"),
    ("WEEKDAY", "serial_number, [return_type]"),
    ("WEEKNUM", "serial_number, [return_type]"),
    ("EDATE", "start_date, months"),
    ("EOMONTH", "start_date, months"),
    ("DATEDIF", "start_date, end_date, unit"),
    ("DAYS", "end_date, start_date"),
    ("NETWORKDAYS", "start_date, end_date, [holidays]"),
    ("WORKDAY", "start_date, days, [holidays]"),
    ("DATEVALUE", "date_text"),
    ("TIMEVALUE", "time_text"),
    ("YEARFRAC", "start_date, end_date, [basis]"),
    // Information
    ("ISBLANK", "value"),
    ("ISNUMBER", "value"),
    ("ISTEXT", "value"),
    ("ISNONTEXT", "value"),
    ("ISLOGICAL", "value"),
    ("ISERROR", "value"),
    ("ISERR", "value"),
    ("ISNA", "value"),
    ("ISEVEN", "number"),
    ("ISODD", "number"),
    ("NA", ""),
    ("ERROR.TYPE", "error_val"),
    ("TYPE", "value"),
    ("N", "value"),
    ("T", "value"),
];

/// The argument list of function `name` (upper case), if known.
pub fn signature(name: &str) -> Option<&'static str> {
    SIGS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// `NAME(args)` with argument `index` (0-based) marked `«like_this»`; a
/// variadic tail marks its repeating argument.
pub fn hint(name: &str, index: usize) -> Option<String> {
    let sig = signature(name)?;
    if sig.is_empty() {
        return Some(format!("{name}()"));
    }
    let parts: Vec<&str> = sig.split(", ").collect();
    let repeat = parts.iter().position(|p| *p == "…");
    let mark = match repeat {
        // Past the listed arguments of a variadic list: mark the one that
        // repeats (the argument just before `…`).
        Some(rep) if index >= rep => rep.saturating_sub(1),
        _ => index,
    };
    let shown: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i == mark {
                format!("«{p}»")
            } else {
                (*p).to_string()
            }
        })
        .collect();
    Some(format!("{name}({})", shown.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_function_has_a_signature() {
        for f in super::super::all() {
            assert!(signature(f.name).is_some(), "no signature for {}", f.name);
        }
        for (name, _, _) in super::super::SPECIAL {
            assert!(signature(name).is_some(), "no signature for {name}");
        }
    }

    #[test]
    fn hints_mark_the_current_argument() {
        assert_eq!(
            hint("SUMIF", 1).as_deref(),
            Some("SUMIF(range, «criteria», [sum_range])")
        );
        assert_eq!(
            hint("SUM", 5).as_deref(),
            Some("SUM(number1, «[number2]», …)")
        );
        assert_eq!(hint("PI", 0).as_deref(), Some("PI()"));
    }
}
