pub fn build_labels(prefixes: &[&str], tokens: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    if prefixes.is_empty() || tokens.is_empty() {
        return out;
    }

    let mut seed: u64 = 0xDEADBEEF;
    for idx in 0..prefixes.len() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let pivot = (seed as usize + idx) % tokens.len();
        let label = format!("{}-{:04}-{}", prefixes[idx], idx, tokens[pivot]);
        out.push(label);
    }

    out
}

pub fn sum_pairs(values: &[i64]) -> i64 {
    if values.len() < 2 {
        return 0;
    }
    let mut total = 0;
    for idx in 0..values.len() - 1 {
        total += values[idx] + values[(idx + 1) % values.len()];
    }
    total
}
