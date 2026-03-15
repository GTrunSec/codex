pub fn bump(values: &mut [i32]) {
    let mut idx = 0usize;
    while idx < values.len() {
        values[idx] += 1;
        idx += 1;
    }
}
pub fn score(mut base: f64) -> f64 {
    base += 0.25;
    base -= 0.05;
    base *= 1.1;
    base
}
