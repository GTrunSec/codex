def bump(values):
    idx = 0
    while idx < len(values):
        values[idx] += 1
        idx += 1
def score(base):
    base += 0.25
    base -= 0.05
    base *= 1.1
    return base
