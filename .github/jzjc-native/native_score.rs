//! Native, closed-set ModelTrace scorer. No Python interpreter or NumPy runtime.
//! Based on xqy2006/ModelTrace commit 6cd4a8ff0f3f4e49d6d6969dd2fbadfa1569fde2.
//! Scores are relative to the enrolled fingerprint bank; not proof of model identity.

use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, path::Path, sync::OnceLock};

const DIM: usize = 355;
const ALPHA: f64 = 0.5;
const MAX_BANK_BYTES: u64 = 20 * 1024 * 1024;
type Calc<T> = Result<T, String>;

#[derive(Clone, Deserialize)]
struct Model {
    id: String,
    display_name: String,
    family: String,
    #[serde(default)]
    family_name: Option<String>,
    counts: Vec<f64>,
}
#[derive(Clone, Deserialize)]
struct Feature {
    feature_mean: Vec<f64>,
    feature_scale: Vec<f64>,
    nuisance_basis: Vec<Vec<f64>>,
    centroids: Vec<Vec<f64>>,
}
#[derive(Clone, Deserialize)]
struct OrderedFeature {
    feature_mean: Vec<f64>,
    feature_scale: Vec<f64>,
    nuisance_basis: Vec<Vec<f64>>,
    centroids: Vec<Vec<f64>>,
    environment_centroids: Vec<Vec<Vec<f64>>>,
    weight: f64,
}
#[derive(Clone, Deserialize)]
struct Robust {
    model_order: Vec<String>,
    hellinger: Feature,
    ordered_blocks: OrderedFeature,
}
#[derive(Clone, Deserialize)]
struct Calibration {
    beta: f64,
    cv_accuracy: f64,
}
#[derive(Clone, Deserialize)]
struct Bank {
    schema: Value,
    method: Value,
    built_at: String,
    models: Vec<Model>,
    robust: Robust,
    calibration: BTreeMap<String, Calibration>,
}
fn builtin() -> &'static Bank {
    static BANK: OnceLock<Bank> = OnceLock::new();
    BANK.get_or_init(|| {
        serde_json::from_str(include_str!("../../vendor/modeltrace/data/unified_bank.json"))
            .expect("ModelTrace bundled bank must be valid")
    })
}
fn read_bank(path: &str) -> Calc<Bank> {
    let path = Path::new(path);
    let metadata = std::fs::metadata(path).map_err(|_| "指纹库文件不存在".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_BANK_BYTES {
        return Err("指纹库不是普通文件或超过20MiB".into());
    }
    let bytes = std::fs::read(path).map_err(|_| "指纹库读取失败".to_owned())?;
    serde_json::from_slice(&bytes).map_err(|e| format!("指纹库JSON格式错误：{e}"))
}
fn finite_vector(v: &[f64], dimension: usize, field: &str) -> Calc<()> {
    if v.len() != dimension || v.iter().any(|x| !x.is_finite()) {
        return Err(format!("指纹库矩阵无效：{field}"));
    }
    Ok(())
}
fn validate_feature(
    mean: &[f64], scale: &[f64], basis: &[Vec<f64>],
    centroids: &[Vec<f64>], dim: usize, model_count: usize, field: &str
) -> Calc<()> {
    finite_vector(mean, dim, field)?;
    finite_vector(scale, dim, field)?;
    if scale.iter().any(|x| *x <= 0.0) { return Err("指纹标准化尺度必须为正数".into()); }
    if basis.len() > dim { return Err("指纹库干扰方向维数无效".into()); }
    for row in basis { finite_vector(row, dim, field)?; }
    if centroids.len() != model_count { return Err("指纹模型中心数不一致".into()); }
    for row in centroids { finite_vector(row, dim, field)?; }
    Ok(())
}
fn validate(bank: &Bank) -> Calc<()> {
    let reference = builtin();
    if bank.schema != reference.schema { return Err("指纹库schema不兼容".into()); }
    let reference_method = reference.method.as_object().ok_or("参考库方法无效")?;
    for (key, value) in reference_method {
        if bank.method.get(key) != Some(value) {
            return Err("指纹方法发生变更；需要升级插件评分器".into());
        }
    }
    let count = bank.models.len();
    if !(2..=128).contains(&count) { return Err("指纹库应包含2至128个模型".into()); }
    let mut ids = BTreeSet::new();
    for model in &bank.models {
        if model.id.is_empty() || model.id.len() > 128 || !ids.insert(&model.id)
            || model.display_name.len() > 128 || model.family.len() > 64
        {
            return Err("指纹模型ID或名称无效".into());
        }
        finite_vector(&model.counts, DIM, "counts")?;
        if model.counts.iter().any(|c| *c < 0.0) || model.counts.iter().sum::<f64>() <= 0.0 {
            return Err("指纹模型计数无效".into());
        }
    }
    if bank.robust.model_order != bank.models.iter().map(|m| m.id.clone()).collect::<Vec<_>>() {
        return Err("指纹模型顺序不一致".into());
    }
    let h = &bank.robust.hellinger;
    validate_feature(&h.feature_mean, &h.feature_scale, &h.nuisance_basis,
                     &h.centroids, DIM, count, "hellinger")?;
    let o = &bank.robust.ordered_blocks;
    validate_feature(&o.feature_mean, &o.feature_scale, &o.nuisance_basis,
                     &o.centroids, 74, count, "ordered_blocks")?;
    if o.environment_centroids.is_empty() || o.environment_centroids.len() > 64 {
        return Err("指纹环境数量无效".into());
    }
    for env in &o.environment_centroids {
        if env.len() != count { return Err("指纹环境模型中心数量无效".into()); }
        for row in env { finite_vector(row, 74, "ordered_blocks.environments")?; }
    }
    if !o.weight.is_finite() || !(0.0..=1.0).contains(&o.weight)
        || o.weight != reference.robust.ordered_blocks.weight {
        return Err("指纹融合权重已变化，请升级插件".into());
    }
    for key in ["1", "2", "3"] {
        let c = bank.calibration.get(key).ok_or("缺少指纹校准参数")?;
        if !c.beta.is_finite() || !(0.0..=10000.0).contains(&c.beta) || c.beta == 0.0
            || !c.cv_accuracy.is_finite() || !(0.0..=1.0).contains(&c.cv_accuracy) {
            return Err("指纹校准参数无效".into());
        }
    }
    Ok(())
}
fn min_numbers(expected: usize) -> usize { 80.max((expected * 55).div_ceil(100)) }

// Matches ModelTrace's "longest numeric run" parser for the ASCII digits in probes.
fn parse_numbers(text: &str) -> Vec<u16> {
    let mut longest = Vec::new();
    let mut current = Vec::new();
    let mut digits = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if !digits.is_empty() {
            if let Ok(n) = digits.parse::<u16>() {
                if (1..=355).contains(&n) { current.push(n); }
            }
            digits.clear();
        }
        if ch.is_alphabetic() {
            if current.len() > longest.len() { longest = std::mem::take(&mut current); }
            current.clear();
        }
    }
    if current.len() > longest.len() { current } else { longest }
}
fn has_cycle(values: &[u16]) -> bool {
    for size in 1..=48.min(values.len() / 3) {
        if values.iter().enumerate().all(|(i, v)| *v == values[i % size]) {
            return true;
        }
    }
    false
}
fn dot(x: &[f64], y: &[f64]) -> f64 {
    x.iter().zip(y).map(|(a, b)| a * b).sum()
}
fn standardize(values: &[f64]) -> Vec<f64> {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    let scale = variance.sqrt().max(1e-12);
    values.iter().map(|v| (v - mean) / scale).collect()
}
fn divide_by_norm(mut values: Vec<f64>) -> Vec<f64> {
    let norm = dot(&values, &values).sqrt().max(1e-12);
    for v in &mut values { *v /= norm; }
    values
}
fn subtract_nuisance(values: &mut [f64], basis: &[Vec<f64>]) {
    // Compute the projection coefficients from the ORIGINAL vector, as in NumPy.
    let coefficients: Vec<f64> = basis.iter().map(|row| dot(values, row)).collect();
    for (row, coeff) in basis.iter().zip(coefficients) {
        for (v, b) in values.iter_mut().zip(row) { *v -= coeff * b; }
    }
}
fn normalized_feature(values: &[f64], mean: &[f64], scale: &[f64]) -> Vec<f64> {
    values.iter().zip(mean).zip(scale).map(|((v, m), s)| (v-m)/s).collect()
}
fn compare_centroids(feature: &[f64], centroids: &[Vec<f64>]) -> Vec<f64> {
    centroids.iter().map(|center| dot(feature, center)).collect()
}
fn hellinger_feature(numbers: &[u16]) -> Vec<f64> {
    let mut counts = vec![ALPHA; DIM];
    for n in numbers { counts[usize::from(*n - 1)] += 1.0; }
    let sum: f64 = counts.iter().sum();
    counts.iter().map(|v| (v / sum).sqrt()).collect()
}
fn ordered_feature(numbers: &[u16]) -> Vec<f64> {
    let mut feature = Vec::with_capacity(74);
    let n = numbers.len();
    let mut start = 0;
    for quarter in 0..4 {
        let size = n / 4 + usize::from(quarter < n % 4);
        let mut counts = [ALPHA; 16];
        for number in &numbers[start..start + size] {
            // NumPy histogram bins=16 range=(1,356): end point 356 is exclusive.
            let bin = ((usize::from(*number) - 1) * 16 / 355).min(15);
            counts[bin] += 1.0;
        }
        start += size;
        let sum: f64 = counts.iter().sum();
        feature.extend(counts.iter().map(|v| (v / sum).sqrt()));
    }
    let mut last_digits = [ALPHA; 10];
    for number in numbers { last_digits[usize::from(*number % 10)] += 1.0; }
    let sum: f64 = last_digits.iter().sum();
    feature.extend(last_digits.iter().map(|v| (v / sum).sqrt()));
    feature
}
fn hellinger_scores(numbers: &[u16], bank: &Bank) -> (Vec<f64>, Vec<f64>) {
    let f = &bank.robust.hellinger;
    let mut projected = normalized_feature(
        &hellinger_feature(numbers), &f.feature_mean, &f.feature_scale
    );
    subtract_nuisance(&mut projected, &f.nuisance_basis);
    let projected = divide_by_norm(projected);
    let nuisance = standardize(&compare_centroids(&projected, &f.centroids));
    (standardize(&nuisance), nuisance)
}
fn ordered_scores(numbers: &[u16], bank: &Bank) -> Vec<f64> {
    let b = &bank.robust.ordered_blocks;
    let standardized = normalized_feature(&ordered_feature(numbers), &b.feature_mean, &b.feature_scale);
    let normalized = divide_by_norm(standardized.clone());
    let mut template = vec![f64::NEG_INFINITY; bank.models.len()];
    for environment in &b.environment_centroids {
        for (index, centroid) in environment.iter().enumerate() {
            template[index] = template[index].max(dot(&normalized, centroid));
        }
    }
    let template = standardize(&template);
    let mut projected = standardized;
    subtract_nuisance(&mut projected, &b.nuisance_basis);
    let projected = divide_by_norm(projected);
    let nuisance = standardize(&compare_centroids(&projected, &b.centroids));
    let blend: Vec<f64> = template.iter().zip(&nuisance)
        .map(|(t, n)| 0.5 * t + 0.5 * n).collect();
    standardize(&blend)
}
fn model_scores(numbers: &[u16], bank: &Bank) -> Vec<f64> {
    let (marginal, _) = hellinger_scores(numbers, bank);
    let w = bank.robust.ordered_blocks.weight;
    if w == 0.0 { return marginal; }
    let ordered = ordered_scores(numbers, bank);
    marginal.iter().zip(ordered).map(|(m, o)| (1.0-w)*m + w*o).collect()
}
fn classify_score(numbers: &[u16], bank: &Bank) -> Calc<Value> {
    if numbers.len() < 80 || numbers.len() > 4096
        || numbers.iter().any(|n| !(1..=355).contains(n)) {
        return Err("数字指纹长度或范围无效".into());
    }
    if has_cycle(numbers) { return Err("数字指纹包含周期性重复区块；不能用于模型归因".into()); }
    let scores = model_scores(numbers, bank);
    let beta = bank.calibration.get("1").ok_or("校准参数缺失")?.beta;
    let peak = scores.iter().map(|s| beta * s).fold(f64::NEG_INFINITY, f64::max);
    let weights: Vec<f64> = scores.iter().map(|s| (beta * s - peak).exp()).collect();
    let total: f64 = weights.iter().sum();
    if !total.is_finite() || total <= 0.0 { return Err("评分结果非有限数".into()); }
    let probs: Vec<f64> = weights.iter().map(|w| w / total).collect();

    let mut sorted: Vec<usize> = (0..bank.models.len()).collect();
    sorted.sort_by(|a, b| probs[*b].total_cmp(&probs[*a]).then_with(|| a.cmp(b)));
    let first = sorted[0];
    let results: Vec<Value> = sorted.iter().map(|i| {
        let model = &bank.models[*i];
        json!({"model":model.id,"label":model.display_name,
               "family":model.family,"probability":probs[*i]})
    }).collect();

    let mut order = Vec::<String>::new();
    let mut families = BTreeMap::<String, (String, f64)>::new();
    for (m, p) in bank.models.iter().zip(&probs) {
        let family = if m.family.is_empty() { "models" } else { &m.family };
        let label = m.family_name.as_deref().filter(|x| !x.is_empty()).unwrap_or(match family {
            "gpt" => "GPT", "claude" => "Claude", _ => family,
        });
        if !families.contains_key(family) { order.push(family.to_owned()); }
        let entry = families.entry(family.to_owned()).or_insert((label.to_owned(), 0.0));
        entry.1 += p;
    }
    let family_results: Vec<Value> = order.iter().filter_map(|family| {
        families.get(family).map(|(label, prob)| json!({
            "family":family,"display_name":label,"probability":prob
        }))
    }).collect();
    Ok(json!({
        "prediction":bank.models[first].id,
        "probability":probs[first],
        "results":results,
        "families":family_results,
    }))
}
fn next_challenge() -> Value {
    let uuid = uuid::Uuid::new_v4();
    let seed = uuid.as_u128();
    let length = 292 + (seed % 41) as usize;
    let openings = ["这是一次独立的数值选择记录", "请完成下面的无语义整数选择任务",
        "执行一次第一反应取值记录", "生成一组不承载语义的整数选择"];
    let opener = openings[(seed as usize) % openings.len()];
    let prompt = format!(
        "{opener}。逐项选择 {length} 个 1 到 355（含端点）的整数。每个位置都要单独选择；不要连续递增或递减，\
         也不要采用等差、循环、重复区块或其他规则化模式。必须由当前语言模型直接完成：\
         禁止调用工具、搜索、代码、计算器或随机数生成器。重复值可以保留。\
         只输出逗号或空格分隔的数字，不要开场白。"
    );
    json!([{"id":format!("probe-1-{}",uuid.simple()),"expected_count":length,"prompt":prompt}])
}
fn path<'a>(input: &'a Value) -> Calc<&'a str> {
    input.get("path").and_then(Value::as_str).ok_or("缺少指纹库路径".into())
}
pub fn handle(input: Value) -> Calc<Value> {
    let op = input.get("op").and_then(Value::as_str).ok_or("评分操作缺少op")?;
    match op {
        "challenges" => Ok(next_challenge()),
        "validate" => {
            let bank = read_bank(path(&input)?)?;
            validate(&bank)?;
            // Exercise the numeric scorer to reject incompatible numeric shapes.
            let numbers: Vec<u16> = (0..310).map(|i| ((i*97+i*i)%355+1) as u16).collect();
            let _ = classify_score(&numbers, &bank)?;
            let models: Vec<Value> = bank.models.iter().map(|m| {
                json!({"id":m.id,"label":m.display_name,"family":m.family})
            }).collect();
            Ok(json!({"builtAt":bank.built_at,"models":models}))
        },
        "prepare_score" => {
            let bank = read_bank(path(&input)?)?;
            validate(&bank)?;
            Ok(Value::Null)
        },
        "sample" => {
            let text = input.get("text").and_then(Value::as_str).ok_or("缺少检测回答")?;
            if text.len() > 1024 * 1024 { return Err("检测回答过大".into()); }
            let expected = input.get("expectedCount").and_then(Value::as_u64).ok_or("缺少预期数字数量")? as usize;
            if !(1..=4096).contains(&expected) { return Err("预期数字数量无效".into()); }
            let numbers = parse_numbers(text);
            if numbers.len() < min_numbers(expected) || numbers.len() > 4096 {
                return Err(format!("有效数字不足或过多（{}，至少{}，至多4096）",
                    numbers.len(), min_numbers(expected)));
            }
            if has_cycle(&numbers) { return Err("输出包含周期性重复区块，不能用于归因".into()); }
            Ok(json!(numbers))
        },
        "score" => {
            let bank = read_bank(path(&input)?)?;
            validate(&bank)?;
            let samples = input.get("samples").and_then(Value::as_array).ok_or("评分样本无效")?;
            if samples.len() != 1 { return Err("每轮评分必须有一个有效样本".into()); }
            let numbers: Vec<u16> = serde_json::from_value(
                samples[0].get("numbers").cloned().ok_or("缺少指纹数字")?
            ).map_err(|_| "指纹样本格式错误")?;
            let expected = samples[0].get("expectedCount").and_then(Value::as_u64)
                .ok_or("缺少预期数字数量")? as usize;
            if numbers.len() < min_numbers(expected) { return Err("样本长度不足".into()); }
            classify_score(&numbers, &bank)
        },
        _ => Err("未知评分操作".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_validate_builtin() { validate(builtin()).unwrap(); }
    #[test]
    fn challenge_format_and_different_counts() {
        let challenges = handle(json!({"op":"challenges"})).unwrap();
        assert_eq!(challenges.as_array().unwrap().len(), 1);
        assert!((292..=332).contains(&(challenges[0]["expected_count"].as_u64().unwrap() as usize)));
    }
    #[test]
    fn cycles_are_not_accepted() {
        let text = (0..310).map(|i| (i%5+1).to_string()).collect::<Vec<_>>().join(",");
        assert!(handle(json!({"op":"sample","text":text,"expectedCount":310})).is_err());
    }
    #[test]
    fn probabilities_are_normalized() {
        let numbers: Vec<u16> = (0..310).map(|i| ((i*97+i*i)%355+1) as u16).collect();
        let value = classify_score(&numbers, builtin()).unwrap();
        let total: f64 = value["results"].as_array().unwrap().iter().map(|c| c["probability"].as_f64().unwrap()).sum();
        assert!((total-1.0).abs() < 1e-10);
    }
    #[test]
    fn python_parity_fixture_if_available() {
        let Ok(path) = std::env::var("CPR_PARITY_FIXTURE") else { return; };
        let rows: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for row in rows.as_array().unwrap() {
            let numbers: Vec<u16> = serde_json::from_value(row["numbers"].clone()).unwrap();
            let predicted = classify_score(&numbers, builtin()).unwrap();
            assert_eq!(predicted["prediction"], row["prediction"], "prediction mismatch");
            let expected = row["results"].as_array().unwrap();
            for (ours, original) in predicted["results"].as_array().unwrap().iter().zip(expected) {
                assert_eq!(ours["model"], original["model"], "rank mismatch");
                let difference = (ours["probability"].as_f64().unwrap() - original["probability"].as_f64().unwrap()).abs();
                assert!(difference < 1e-7, "score parity error: {difference}");
            }
        }
    }
}
