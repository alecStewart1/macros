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
