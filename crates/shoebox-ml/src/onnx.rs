//! A small ONNX interpreter on candle, for the face models: just the
//! operators YuNet and SFace use (convolution, pooling, nearest resize,
//! batch norm, PReLU, reshapes and element-wise arithmetic). The protobuf
//! messages are declared here for the fields read, so no `protoc` is needed
//! at build time.

use std::collections::HashMap;

use candle_core::{D, DType, Device, Tensor};
use prost::Message;

use crate::{MlError, Result};

#[derive(Clone, PartialEq, Message)]
struct ModelProto {
    #[prost(message, optional, tag = "7")]
    graph: Option<GraphProto>,
}

#[derive(Clone, PartialEq, Message)]
struct GraphProto {
    #[prost(message, repeated, tag = "1")]
    node: Vec<NodeProto>,
    #[prost(message, repeated, tag = "5")]
    initializer: Vec<TensorProto>,
    #[prost(message, repeated, tag = "11")]
    input: Vec<ValueInfoProto>,
    #[prost(message, repeated, tag = "12")]
    output: Vec<ValueInfoProto>,
}

#[derive(Clone, PartialEq, Message)]
struct NodeProto {
    #[prost(string, repeated, tag = "1")]
    input: Vec<String>,
    #[prost(string, repeated, tag = "2")]
    output: Vec<String>,
    #[prost(string, tag = "4")]
    op_type: String,
    #[prost(message, repeated, tag = "5")]
    attribute: Vec<AttributeProto>,
}

#[derive(Clone, PartialEq, Message)]
struct AttributeProto {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(float, tag = "2")]
    f: f32,
    #[prost(int64, tag = "3")]
    i: i64,
    #[prost(bytes = "vec", tag = "4")]
    s: Vec<u8>,
    #[prost(message, optional, tag = "5")]
    t: Option<TensorProto>,
    #[prost(int64, repeated, tag = "8")]
    ints: Vec<i64>,
}

#[derive(Clone, PartialEq, Message)]
struct TensorProto {
    #[prost(int64, repeated, tag = "1")]
    dims: Vec<i64>,
    #[prost(int32, tag = "2")]
    data_type: i32,
    #[prost(float, repeated, tag = "4")]
    float_data: Vec<f32>,
    #[prost(int64, repeated, tag = "7")]
    int64_data: Vec<i64>,
    #[prost(string, tag = "8")]
    name: String,
    #[prost(bytes = "vec", tag = "9")]
    raw_data: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
struct ValueInfoProto {
    #[prost(string, tag = "1")]
    name: String,
}

fn bad(msg: impl Into<String>) -> MlError {
    MlError::Format(msg.into())
}

/// An initializer or constant: float tensors become candle tensors, int64
/// ones (shapes) stay as integers.
#[derive(Debug, Clone)]
enum Value {
    T(Tensor),
    Ints(Vec<i64>),
}

fn decode_tensor(t: &TensorProto, device: &Device) -> Result<Value> {
    let dims: Vec<usize> = t.dims.iter().map(|&d| d.max(0) as usize).collect();
    if !dims.is_empty() && dims.iter().product::<usize>() == 0 {
        // Empty (e.g. Resize's unused roi): no device buffer for it.
        return Ok(Value::Ints(Vec::new()));
    }
    match t.data_type {
        // FLOAT
        1 => {
            let data: Vec<f32> = if t.raw_data.is_empty() {
                t.float_data.clone()
            } else {
                t.raw_data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&b| f32::from_le_bytes(b))
                    .collect()
            };
            if data.len() != dims.iter().product::<usize>() {
                return Err(bad(format!("{}: size doesn't match its shape", t.name)));
            }
            Ok(Value::T(Tensor::from_vec(data, dims, device)?))
        }
        // INT64
        7 => Ok(Value::Ints(if t.raw_data.is_empty() {
            t.int64_data.clone()
        } else {
            t.raw_data
                .as_chunks::<8>()
                .0
                .iter()
                .map(|&b| i64::from_le_bytes(b))
                .collect()
        })),
        d => Err(bad(format!("{}: unsupported tensor type {d}", t.name))),
    }
}

/// A loaded model, ready to run.
pub struct Graph {
    nodes: Vec<NodeProto>,
    consts: HashMap<String, Value>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    device: Device,
}

fn attr<'a>(n: &'a NodeProto, name: &str) -> Option<&'a AttributeProto> {
    n.attribute.iter().find(|a| a.name == name)
}

fn ints(n: &NodeProto, name: &str) -> Vec<usize> {
    attr(n, name)
        .map(|a| a.ints.iter().map(|&i| i.max(0) as usize).collect())
        .unwrap_or_default()
}

fn int(n: &NodeProto, name: &str, default: i64) -> i64 {
    attr(n, name).map_or(default, |a| a.i)
}

/// Same padding on both sides of each spatial dim, as candle wants.
fn symmetric(n: &NodeProto) -> Result<usize> {
    let p = ints(n, "pads");
    match p.as_slice() {
        [] => Ok(0),
        [a, b, c, d] if a == b && b == c && c == d => Ok(*a),
        _ => Err(bad(format!("{}: asymmetric padding {p:?}", n.op_type))),
    }
}

impl Graph {
    pub fn load(bytes: &[u8], device: &Device) -> Result<Self> {
        let m = ModelProto::decode(bytes).map_err(|e| bad(format!("ONNX: {e}")))?;
        let g = m.graph.ok_or_else(|| bad("ONNX: no graph"))?;
        let mut consts = HashMap::new();
        for t in &g.initializer {
            consts.insert(t.name.clone(), decode_tensor(t, device)?);
        }
        let mut nodes = Vec::new();
        for n in g.node {
            if n.op_type == "Constant" {
                let t = attr(&n, "value")
                    .and_then(|a| a.t.as_ref())
                    .ok_or_else(|| bad("Constant without a tensor"))?;
                if let Some(out) = n.output.first() {
                    consts.insert(out.clone(), decode_tensor(t, device)?);
                }
            } else {
                nodes.push(n);
            }
        }
        let inputs = g
            .input
            .into_iter()
            .map(|i| i.name)
            .filter(|n| !consts.contains_key(n))
            .collect();
        let outputs = g.output.into_iter().map(|o| o.name).collect();
        Ok(Self {
            nodes,
            consts,
            inputs,
            outputs,
            device: device.clone(),
        })
    }

    /// Run with `input` as the (single) input; returns every graph output.
    pub fn run(&self, input: Tensor) -> Result<HashMap<String, Tensor>> {
        let first = self.inputs.first().ok_or_else(|| bad("ONNX: no input"))?;
        let mut vals: HashMap<&str, Tensor> = HashMap::new();
        vals.insert(first.as_str(), input.to_device(&self.device)?);
        let profile = std::env::var_os("SHOEBOX_ONNX_PROFILE").is_some();
        let mut times: HashMap<String, f64> = HashMap::new();
        for n in &self.nodes {
            let started = std::time::Instant::now();
            let get = |i: usize| -> Result<Tensor> {
                let name = n
                    .input
                    .get(i)
                    .ok_or_else(|| bad(format!("{}: missing input {i}", n.op_type)))?;
                match (vals.get(name.as_str()), self.consts.get(name)) {
                    (Some(t), _) => Ok(t.clone()),
                    (None, Some(Value::T(t))) => Ok(t.clone()),
                    _ => Err(bad(format!("{}: no tensor {name}", n.op_type))),
                }
            };
            let ints_in = |i: usize| -> Result<Vec<i64>> {
                match n.input.get(i).and_then(|name| self.consts.get(name)) {
                    Some(Value::Ints(v)) => Ok(v.clone()),
                    Some(Value::T(t)) => Ok(t
                        .to_dtype(DType::F32)?
                        .flatten_all()?
                        .to_vec1::<f32>()?
                        .into_iter()
                        .map(|x| x as i64)
                        .collect()),
                    None => Err(bad(format!("{}: input {i} must be constant", n.op_type))),
                }
            };
            let out = match n.op_type.as_str() {
                "Conv" => {
                    let x = get(0)?;
                    let w = get(1)?;
                    let stride = ints(n, "strides").first().copied().unwrap_or(1);
                    let dilation = ints(n, "dilations").first().copied().unwrap_or(1);
                    let groups = int(n, "group", 1).max(1) as usize;
                    let pad = symmetric(n)?;
                    let (out_c, in_per_group, _, _) = w.dims4()?;
                    let y = if groups > 1 && in_per_group == 1 && out_c == groups && dilation == 1 {
                        depthwise(&x, &w, pad, stride)?
                    } else {
                        x.conv2d(&w, pad, stride, dilation, groups)?
                    };
                    match n.input.get(2) {
                        Some(_) => y.broadcast_add(&get(2)?.reshape((1, (), 1, 1))?)?,
                        None => y,
                    }
                }
                "Relu" => get(0)?.relu()?,
                "Sigmoid" => candle_nn::ops::sigmoid(&get(0)?)?,
                "Add" => get(0)?.broadcast_add(&get(1)?)?,
                "Sub" => get(0)?.broadcast_sub(&get(1)?)?,
                "Mul" => get(0)?.broadcast_mul(&get(1)?)?,
                "PRelu" => {
                    let x = get(0)?;
                    let slope = get(1)?;
                    let pos = x.relu()?;
                    let neg = x.minimum(0f32)?;
                    pos.broadcast_add(&neg.broadcast_mul(&slope)?)?
                }
                "BatchNormalization" => {
                    let x = get(0)?;
                    let eps = attr(n, "epsilon").map_or(1e-5, |a| a.f);
                    // Per channel (dim 1), for any rank.
                    let rank = x.rank();
                    let shape = |t: Tensor| {
                        let mut d = vec![1usize; rank];
                        d[1] = t.elem_count();
                        t.reshape(d)
                    };
                    let (scale, bias, mean, var) = (
                        shape(get(1)?)?,
                        shape(get(2)?)?,
                        shape(get(3)?)?,
                        shape(get(4)?)?,
                    );
                    let inv = (var + f64::from(eps))?.sqrt()?.recip()?;
                    x.broadcast_sub(&mean)?
                        .broadcast_mul(&inv.broadcast_mul(&scale)?)?
                        .broadcast_add(&bias)?
                }
                "MaxPool" => {
                    let k = ints(n, "kernel_shape");
                    let s = ints(n, "strides");
                    if symmetric(n)? != 0 || int(n, "ceil_mode", 0) != 0 {
                        return Err(bad("MaxPool: padding / ceil mode unsupported"));
                    }
                    match (k.as_slice(), s.as_slice()) {
                        ([kh, kw], [sh, sw]) => {
                            get(0)?.max_pool2d_with_stride((*kh, *kw), (*sh, *sw))?
                        }
                        _ => return Err(bad("MaxPool: expected 2-D kernel and strides")),
                    }
                }
                "Resize" => {
                    let mode = attr(n, "mode").map(|a| a.s.as_slice());
                    if mode != Some(b"nearest".as_slice()) {
                        return Err(bad("Resize: only nearest is supported"));
                    }
                    let x = get(0)?;
                    let (_, _, h, w) = x.dims4()?;
                    let scales = match n.input.get(2).and_then(|s| self.consts.get(s)) {
                        Some(Value::T(t)) => t.to_vec1::<f32>()?,
                        _ => Vec::new(),
                    };
                    let (oh, ow) = match scales.as_slice() {
                        [_, _, sh, sw] => (
                            (h as f32 * sh).floor() as usize,
                            (w as f32 * sw).floor() as usize,
                        ),
                        _ => {
                            let sz = ints_in(3)?;
                            match sz.as_slice() {
                                [_, _, a, b] => (*a as usize, *b as usize),
                                _ => return Err(bad("Resize: no scales or sizes")),
                            }
                        }
                    };
                    x.upsample_nearest2d(oh, ow)?
                }
                "Transpose" => {
                    let perm = ints(n, "perm");
                    get(0)?.permute(perm)?.contiguous()?
                }
                "Reshape" => {
                    let x = get(0)?;
                    let spec = ints_in(1)?;
                    let dims = x.dims().to_vec();
                    let mut shape: Vec<usize> = Vec::with_capacity(spec.len());
                    let mut infer = None;
                    for (i, &d) in spec.iter().enumerate() {
                        match d {
                            0 => shape.push(dims.get(i).copied().unwrap_or(1)),
                            -1 => {
                                infer = Some(i);
                                shape.push(1);
                            }
                            d => shape.push(d as usize),
                        }
                    }
                    if let Some(i) = infer {
                        let known: usize = shape.iter().product();
                        shape[i] = x.elem_count() / known.max(1);
                    }
                    x.contiguous()?.reshape(shape)?
                }
                "Flatten" => {
                    let x = get(0)?;
                    let axis = int(n, "axis", 1).max(0) as usize;
                    let lead: usize = x.dims()[..axis].iter().product();
                    x.contiguous()?.reshape((lead, ()))?
                }
                "Gemm" => {
                    let a = get(0)?;
                    let b = get(1)?;
                    let b = if int(n, "transB", 0) != 0 { b.t()? } else { b };
                    let alpha = attr(n, "alpha").map_or(1.0, |a| a.f);
                    let beta = attr(n, "beta").map_or(1.0, |a| a.f);
                    let mut y = (a.matmul(&b)? * f64::from(alpha))?;
                    if n.input.get(2).is_some() {
                        y = y.broadcast_add(&(get(2)? * f64::from(beta))?)?;
                    }
                    y
                }
                "Dropout" | "Identity" => get(0)?,
                op => return Err(bad(format!("ONNX operator {op} isn't supported"))),
            };
            if profile {
                let _ = out
                    .flatten_all()
                    .and_then(|t| t.sum_all())
                    .and_then(|t| t.to_scalar::<f32>());
                let mut key = n.op_type.clone();
                if n.op_type == "Conv" {
                    key = format!("Conv g{} {:?}", int(n, "group", 1), ints(n, "kernel_shape"));
                }
                *times.entry(key).or_default() += started.elapsed().as_secs_f64() * 1e3;
            }
            if let Some(name) = n.output.first() {
                vals.insert(name.as_str(), out);
            }
        }
        if profile {
            let mut t: Vec<_> = times.into_iter().collect();
            t.sort_by(|a, b| b.1.total_cmp(&a.1));
            eprintln!("onnx profile: {t:?}");
        }
        self.outputs
            .iter()
            .map(|o| {
                vals.get(o.as_str())
                    .cloned()
                    .map(|t| (o.clone(), t))
                    .ok_or_else(|| bad(format!("ONNX: output {o} not computed")))
            })
            .collect()
    }
}

/// A depthwise convolution (one filter per channel), as a sum of shifted,
/// scaled copies of the input: candle runs grouped convolutions one group at
/// a time, which for hundreds of channels is far slower.
fn depthwise(x: &Tensor, w: &Tensor, pad: usize, stride: usize) -> Result<Tensor> {
    let (b, c, h, wd) = x.dims4()?;
    let (_, _, kh, kw) = w.dims4()?;
    let x = x.pad_with_zeros(2, pad, pad)?.pad_with_zeros(3, pad, pad)?;
    // Stride 1 first; a stride is a subsample of that.
    let (h1, w1) = (h + 2 * pad - kh + 1, wd + 2 * pad - kw + 1);
    let mut y: Option<Tensor> = None;
    for i in 0..kh {
        for j in 0..kw {
            let part = x.narrow(2, i, h1)?.narrow(3, j, w1)?.contiguous()?;
            let k = w.narrow(2, i, 1)?.narrow(3, j, 1)?.reshape((1, c, 1, 1))?;
            let term = part.broadcast_mul(&k)?;
            y = Some(match y {
                None => term,
                Some(acc) => (acc + term)?,
            });
        }
    }
    let y = y.ok_or_else(|| bad("depthwise: empty kernel"))?;
    if stride == 1 {
        return Ok(y);
    }
    let (oh, ow) = ((h1 - 1) / stride + 1, (w1 - 1) / stride + 1);
    Ok(y.pad_with_zeros(2, 0, oh * stride - h1)?
        .pad_with_zeros(3, 0, ow * stride - w1)?
        .reshape((b, c, oh, stride, ow, stride))?
        .narrow(3, 0, 1)?
        .narrow(5, 0, 1)?
        .reshape((b, c, oh, ow))?)
}

/// Unit-length rows of a (batch, n) tensor.
pub fn unit_rows(t: &Tensor) -> Result<Vec<Vec<f32>>> {
    let n = t.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?;
    Ok(t.broadcast_div(&n)?.to_vec2::<f32>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depthwise_matches_grouped_conv() -> anyhow::Result<()> {
        let dev = Device::Cpu;
        for (h, w, stride) in [(7, 9, 1), (8, 8, 2), (9, 7, 2)] {
            let x = Tensor::randn(0f32, 1.0, (2, 5, h, w), &dev)?;
            let k = Tensor::randn(0f32, 1.0, (5, 1, 3, 3), &dev)?;
            let want = x.conv2d(&k, 1, stride, 1, 5)?;
            let got = depthwise(&x, &k, 1, stride)?;
            assert_eq!(want.dims(), got.dims());
            let diff = (want - got)?.abs()?.max_all()?.to_scalar::<f32>()?;
            assert!(diff < 1e-4, "{h}x{w} stride {stride}: {diff}");
        }
        Ok(())
    }
}
