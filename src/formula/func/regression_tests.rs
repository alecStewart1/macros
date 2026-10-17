use super::testing::Mock;
use crate::formula::value::Value;

fn f(m: &mut Mock, src: &str) -> Value {
    let e = crate::formula::parse::parse(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    crate::formula::eval::eval_cell(&e, m)
}

#[test]
fn regression_1904_text_date_coercion() {
    let mut m = Mock::new();
    m.d1904 = true;
    let dv = f(&mut m, "=DATEVALUE(\"2024-01-01\")");
    assert_eq!(f(&mut m, "=\"2024-01-01\"+0"), dv);
}

#[test]
fn regression_1904_abs() {
    let mut m = Mock::new();
    m.d1904 = true;
    let dv = f(&mut m, "=DATEVALUE(\"2024-01-01\")");
    assert_eq!(f(&mut m, "=ABS(\"2024-01-01\")"), dv);
}

#[test]
fn regression_whole_column_array_depends_on_formula_row() {
    let mut m = Mock::grid(&[&["1"], &["2"], &["3"]]);
    m.here = (0, 3);
    let top = f(&mut m, "=SUMPRODUCT((A:A<5)*1)");
    m.here = (99, 3);
    let low = f(&mut m, "=SUMPRODUCT((A:A<5)*1)");
    assert_eq!(top, low, "same formula, different row");
}

#[test]
fn regression_whole_column_min() {
    let mut m = Mock::grid(&[&["1"], &["2"], &["3"]]);
    m.here = (0, 3);
    let top = f(&mut m, "=MIN(A:A+0)");
    m.here = (20, 3);
    let low = f(&mut m, "=MIN(A:A+0)");
    assert_eq!(top, low);
}

#[test]
fn regression_series_i128_step_overflow() {
    let big = "170141183460469231731687303715884105727"; // i128::MAX
    let neg = "-170141183460469231731687303715884105727";
    let out = crate::grid::series::next_in_series(Some(neg), big, 1);
    eprintln!("{out}");
}

#[test]
fn regression_sum_empty_if() {
    let mut m = Mock::grid(&[&["1"], &["2"], &["3"]]);
    eprintln!("{:?}", f(&mut m, "=SUM(IF(A1:A3>1,B1:B3))"));
    eprintln!("{:?}", f(&mut m, "=SUM(B1:B3)"));
    eprintln!("{:?}", f(&mut m, "=SUM(IF(A1:A3>1,B1:B3))&\"\""));
}

#[test]
fn regression_huge_explicit_ranges_are_num_not_an_abort() {
    use crate::formula::value::ErrorKind;
    let mut m = Mock::grid(&[&["1", "2"], &["3", "4"]]);
    m.here = (0, 5);
    // Each would materialize ~2.6e9 cells.
    for src in [
        "=SUMPRODUCT(A1:Z99999999)",
        "=A1:Z99999999+0",
        "=CONCAT(A1:Z99999999)",
        // Broadcasting a column against a row: 10,000 × 16,384 cells.
        "=SUMPRODUCT(A1:A10000*A1:XFD1)",
    ] {
        assert_eq!(f(&mut m, src), Value::Error(ErrorKind::Num), "{src}");
    }
    // Ordinary ranges are unaffected.
    assert_eq!(f(&mut m, "=SUMPRODUCT(A1:B2)"), Value::Number(10.0));
}

#[test]
fn regression_sorted_lookup_over_mostly_empty_range_is_bounded() {
    let mut m = Mock::grid(&[&["1", "a"], &["5", "b"], &["9", "c"]]);
    m.here = (0, 5);
    let started = std::time::Instant::now();
    assert_eq!(
        f(&mut m, "=VLOOKUP(6,A1:B10000000,2)"),
        Value::Text("b".into())
    );
    assert_eq!(f(&mut m, "=MATCH(6,A1:A10000000,1)"), Value::Number(2.0));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn regression_concat_operator_respects_text_limit() {
    use crate::formula::value::ErrorKind;
    let mut m = Mock::new();
    assert_eq!(
        f(&mut m, "=REPT(\"a\",32767)&\"b\""),
        Value::Error(ErrorKind::Value)
    );
    assert_eq!(
        f(&mut m, "=SUBSTITUTE(REPT(\"a\",32767),\"a\",\"bb\")"),
        Value::Error(ErrorKind::Value)
    );
    assert_eq!(f(&mut m, "=\"a\"&\"b\""), Value::Text("ab".into()));
}

#[test]
fn regression_1904_count_and_choose() {
    let mut m = Mock::new();
    m.d1904 = true;
    let sum = f(&mut m, "=SUM(\"1/1/2024\")");
    assert_eq!(f(&mut m, "=AVERAGEA(\"1/1/2024\")"), sum);
}
