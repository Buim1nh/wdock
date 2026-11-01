//! CPU port of Kawarp's domain warp shader, applied to a pre-blurred thumbnail.
//! Kawarp Copyright (c) 2026 Better Lyrics, MIT; see KAWARP-LICENSE.txt.
fn fract(x: f32) -> f32 { x - x.floor() }
fn mod289(x: f32) -> f32 { x - (x / 289.0).floor() * 289.0 }
fn permute(x: f32) -> f32 { mod289((x * 34.0 + 1.0) * x) }

fn simplex(x: f32, y: f32) -> f32 {
    let skew = (x + y) * 0.3660254;
    let ix = (x + skew).floor(); let iy = (y + skew).floor();
    let unskew = (ix + iy) * 0.21132487;
    let x0 = x - ix + unskew; let y0 = y - iy + unskew;
    let (i1, j1) = if x0 > y0 { (1.0, 0.0) } else { (0.0, 1.0) };
    let points = [(x0, y0, 0.0, 0.0), (x0 + 0.21132487 - i1, y0 + 0.21132487 - j1, i1, j1), (x0 - 0.57735026, y0 - 0.57735026, 1.0, 1.0)];
    let mut sum = 0.0;
    for (px, py, dx, dy) in points {
        let p = permute(permute(mod289(iy) + dy) + mod289(ix) + dx);
        let mut m = (0.5 - px * px - py * py).max(0.0);
        m *= m; m *= m;
        let gx = 2.0 * fract(p * 0.024390243) - 1.0;
        let h = gx.abs() - 0.5;
        let a = gx - (gx + 0.5).floor();
        m *= 1.7928429 - 0.85373473 * (a * a + h * h);
        sum += m * (a * px + h * py);
    }
    130.0 * sum
}

pub fn warp(source: &[u8], width: usize, height: usize, time: f32) -> Vec<u8> {
    if width < 2 || height < 2 || source.len() != width * height * 4 { return source.to_vec(); }
    let mut out = vec![0; source.len()];
    let t = time * 0.05;
    for y in 0..height { for x in 0..width {
        let u = x as f32 / (width - 1) as f32; let v = y as f32 / (height - 1) as f32;
        let distance = ((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt();
        let q = (distance / 0.7).clamp(0.0, 1.0);
        let weight = 1.0 - q * q * (3.0 - 2.0 * q);
        let dx = (simplex(u*0.35+t, v*0.35+t*0.7)*0.65 + simplex(u*0.9+t*1.2+100.0, v*0.9-t)*0.35)*weight;
        let dy = (simplex(u*0.35-t*0.8+50.0, v*0.35+t*0.5+50.0)*0.65 + simplex(u*0.9-t, v*0.9+t*1.1+100.0)*0.35)*weight;
        let sx = ((u + dx).clamp(0.0, 1.0) * (width-1) as f32).clamp(0.0, (width-1) as f32);
        let sy = ((v + dy).clamp(0.0, 1.0) * (height-1) as f32).clamp(0.0, (height-1) as f32);
        let x0 = sx as usize; let y0 = sy as usize; let x1 = (x0+1).min(width-1); let y1 = (y0+1).min(height-1);
        let fx = sx-x0 as f32; let fy = sy-y0 as f32;
        for c in 0..4 {
            let a = source[(y0*width+x0)*4+c] as f32 * (1.0-fx) + source[(y0*width+x1)*4+c] as f32 * fx;
            let b = source[(y1*width+x0)*4+c] as f32 * (1.0-fx) + source[(y1*width+x1)*4+c] as f32 * fx;
            out[(y*width+x)*4+c] = (a*(1.0-fy)+b*fy).round() as u8;
        }
    }}
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn warp_preserves_solid_color_and_moves_gradient() {
        let solid = vec![100; 40*20*4];
        assert_eq!(super::warp(&solid, 40, 20, 3.0), solid);
        let gradient: Vec<u8> = (0..40*20*4).map(|i| ((i / 4) % 40 * 6) as u8).collect();
        assert_ne!(super::warp(&gradient, 40, 20, 0.0), super::warp(&gradient, 40, 20, 15.0));
    }
}
