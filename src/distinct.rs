// This file includes source code from https://github.com/jedisct1/rust-hyperloglog/blob/36d73a2c0a324f4122d32febdb19dd4a815147f0/src/hyperloglog/lib.rs under the following BSD 2-Clause "Simplified" License:
//
// Copyright (c) 2013-2016, Frank Denis
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without modification,
// are permitted provided that the following conditions are met:
//
//   Redistributions of source code must retain the above copyright notice, this
//   list of conditions and the following disclaimer.
//
//   Redistributions in binary form must reproduce the above copyright notice, this
//   list of conditions and the following disclaimer in the documentation and/or
//   other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
// ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
// WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
// ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
// (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
// LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
// ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
// (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
// SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

// This file includes source code from https://github.com/codahale/sketchy/blob/09e9ede8ac27e6fd37d5c5f53ac9b7776c37bc19/src/hyperloglog.rs under the following Apache License 2.0:
//
// Copyright (c) 2015-2017 Coda Hale
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// https://github.com/twitter/algebird/blob/5fdb079447271a5fe0f1fba068e5f86591ccde36/algebird-core/src/main/scala/com/twitter/algebird/HyperLogLog.scala
// https://spark.apache.org/docs/latest/api/scala/index.html#org.apache.spark.rdd.RDD countApproxDistinct
// is_x86_feature_detected ?
use rand::prelude::random;

use serde::{Deserialize, Serialize};
use std::{
	cmp::{self, Ordering}, convert::{identity, TryFrom}, fmt, hash::{Hash, Hasher}, marker::PhantomData, ops::{self, Range}
};
use twox_hash::XxHash;

use super::{f64_to_u8, u64_to_f64, usize_to_f64};
use crate::traits::{Intersect, IntersectPlusUnionIsPlus, New, UnionAssign};

mod consts;
use self::consts::{BIAS_DATA, RAW_ESTIMATE_DATA, TRESHOLD_DATA};

/// Like [`HyperLogLog`] but implements `Ord` and `Eq` by using the estimate of the cardinality.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct HyperLogLogMagnitude<V>(HyperLogLog<V>);
impl<V: Hash> Ord for HyperLogLogMagnitude<V> {
	#[inline(always)]
	fn cmp(&self, other: &Self) -> Ordering {
		self.0.len().partial_cmp(&other.0.len()).unwrap()
	}
}
impl<V: Hash> PartialOrd for HyperLogLogMagnitude<V> {
	#[inline(always)]
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		self.0.len().partial_cmp(&other.0.len())
	}
}
impl<V: Hash> PartialEq for HyperLogLogMagnitude<V> {
	#[inline(always)]
	fn eq(&self, other: &Self) -> bool {
		self.0.len().eq(&other.0.len())
	}
}
impl<V: Hash> Eq for HyperLogLogMagnitude<V> {}
impl<V: Hash> Clone for HyperLogLogMagnitude<V> {
	fn clone(&self) -> Self {
		Self(self.0.clone())
	}
}
impl<V: Hash> New for HyperLogLogMagnitude<V> {
	type Config = f64;
	fn new(config: &Self::Config) -> Self {
		Self(New::new(config))
	}
}

impl<V: Hash> Intersect for HyperLogLogMagnitude<V> {
	fn intersect<'a>(iter: impl Iterator<Item = &'a Self>) -> Option<Self>
	where
		Self: Sized + 'a,
	{
		Intersect::intersect(iter.map(|x| &x.0)).map(Self)
	}
}
impl<'a, V: Hash> UnionAssign<&'a HyperLogLogMagnitude<V>> for HyperLogLogMagnitude<V> {
	fn union_assign(&mut self, rhs: &'a Self) {
		self.0.union_assign(&rhs.0)
	}
}
impl<'a, V: Hash> ops::AddAssign<&'a V> for HyperLogLogMagnitude<V> {
	fn add_assign(&mut self, rhs: &'a V) {
		self.0.add_assign(rhs)
	}
}
impl<'a, V: Hash> ops::AddAssign<&'a Self> for HyperLogLogMagnitude<V> {
	fn add_assign(&mut self, rhs: &'a Self) {
		self.0.add_assign(&rhs.0)
	}
}
impl<V: Hash> fmt::Debug for HyperLogLogMagnitude<V> {
	fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
		self.0.fmt(fmt)
	}
}
impl<V> IntersectPlusUnionIsPlus for HyperLogLogMagnitude<V> {
	const VAL: bool = <HyperLogLog<V> as IntersectPlusUnionIsPlus>::VAL;
}

/// An implementation of the [HyperLogLog](https://en.wikipedia.org/wiki/HyperLogLog) data structure with *bias correction*.
///
/// See [*HyperLogLog: the analysis of a near-optimal cardinality estimation algorithm*](http://algo.inria.fr/flajolet/Publications/FlFuGaMe07.pdf) and [*HyperLogLog in Practice: Algorithmic Engineering of a State of The Art Cardinality Estimation Algorithm*](https://ai.google/research/pubs/pub40671) for background on HyperLogLog with bias correction.
/// HyperLogLog support of delete operation refer to:
/// [Every Row Counts: Combining Sketches and Sampling for Accurate Group-By Result Estimates](https://db.in.tum.de/~freitag/papers/p23-freitag-cidr19.pdf)
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct HyperLogLog<V: ?Sized> {
	alpha: f64,
	zero: usize,
	sum: f64,
	p: u8,
	m: Box<[u8]>,
	counters: Option<Vec<Box<[u8]>>>,
	marker: PhantomData<fn(V)>,
}

impl<V: ?Sized> HyperLogLog<V>
where
	V: Hash,
{
	/// Create an empty `HyperLogLog` data structure with the specified error tolerance.
	pub fn new(error_rate: f64) -> Self {
		assert!(0.0 < error_rate && error_rate < 1.0);
		let p = f64_to_u8((f64::log2(1.04 / error_rate) * 2.0).ceil());
		assert!(0 < p && p < 64);
		let alpha = Self::get_alpha(p);
		Self {
			alpha,
			zero: 1 << p,
			sum: f64::from(1 << p),
			p,
			m: vec![0; 1 << p].into_boxed_slice(),
			counters: None,
			marker: PhantomData,
		}
	}

	/// Create an empty `HyperLogLog` data structure with the specified error tolerance.
	/// Also create a counters to support delete operation.
	pub fn new_with_counters(error_rate: f64) -> Self {
		assert!(0.0 < error_rate && error_rate < 1.0);
		let p = f64_to_u8((f64::log2(1.04 / error_rate) * 2.0).ceil());
		assert!(0 < p && p < 64);
		let alpha = Self::get_alpha(p);
		let max_width = 64 - p as usize;
		Self {
			alpha,
			zero: 1 << p,
			sum: f64::from(1 << p),
			p,
			m: vec![0; 1 << p].into_boxed_slice(),
			counters: Some(vec![vec![0; max_width].into_boxed_slice(); 1 << p]),
			marker: PhantomData,
		}
	}

	/// Create an empty `HyperLogLog` data structure, copying the error tolerance from `hll`.
	pub fn new_from(hll: &Self) -> Self {
		Self {
			alpha: hll.alpha,
			zero: hll.m.len(),
			sum: usize_to_f64(hll.m.len()),
			p: hll.p,
			m: vec![0; hll.m.len()].into_boxed_slice(),
			counters: hll.counters.clone(),
			marker: PhantomData,
		}
	}

	#[inline]
	fn is_change_power(power: u8) -> bool {
		assert!(power >= 1);
		let p = u64::from(power);
		if p >= u64::BITS as u64 {
			return false;
		}
		random::<u64>() % (2 << (p - 1)) == 0
	}

	/// "Visit" an element.
	#[inline]
	pub fn push(&mut self, value: &V) {
		let mut hasher = XxHash::default();
		value.hash(&mut hasher);
		let x = hasher.finish();
		let j = x & (self.m.len() as u64 - 1);
		let index = usize::try_from(j).unwrap();
		let w = x >> self.p;
		let rho = Self::get_rho(w, 64 - self.p);
		let mjr = &mut self.m[index];
		let old = *mjr;
		let new = cmp::max(old, rho);
		self.zero -= if old == 0 { 1 } else { 0 };

		// see pow_bithack()
		self.sum -= f64::from_bits(u64::max_value().wrapping_sub(u64::from(old)) << 54 >> 2)
			- f64::from_bits(u64::max_value().wrapping_sub(u64::from(new)) << 54 >> 2);

		// update counters
		if let Some(counters) = &mut self.counters {
			let c = &mut counters[index];
			let now_counter = &mut c[new as usize];
			if *now_counter <= 128 {
				*now_counter += 1;
			} else {
				if Self::is_change_power(*now_counter - 128) {
					*now_counter += 1;
				}
			}
		}

		*mjr = new;
	}

	/// "Remove" an element.
	#[inline]
	pub fn delete(&mut self, value: &V) {
		let max_width = 64 - self.p;
		if let Some(counters) = &mut self.counters {
			let mut hasher = XxHash::default();
			value.hash(&mut hasher);
			let x = hasher.finish();
			let j = x & (self.m.len() as u64 - 1);
			let index = usize::try_from(j).unwrap();
			let w = x >> self.p;
			let rho = Self::get_rho(w, max_width);
			let c = &mut counters[index];
			let old_counter = &mut c[rho as usize];
			if *old_counter >= 1 {
				if *old_counter <= 128 {
					*old_counter -= 1;
				} else {
					if Self::is_change_power(*old_counter - 128) {
						*old_counter -= 1;
					}
				}

				// If counter reach zero, update bucket.
				if *old_counter == 0 {
					self.zero += if rho != 0 { 1 } else { 0 };
					// see pow_bithack()
					self.sum -=
						f64::from_bits(u64::max_value().wrapping_sub(u64::from(rho)) << 54 >> 2);
					// Find the biggest value less than rho
					let mjr = &mut self.m[index];
					for i in (0..rho - 1).rev() {
						if c[i as usize] > 0 {
							self.zero -= if i != 0 { 1 } else { 0 };
							// see pow_bithack()
							self.sum += f64::from_bits(
								u64::max_value().wrapping_sub(u64::from(i)) << 54 >> 2,
							);
							*mjr = i;
							return;
						}
					}
					*mjr = 0;
				}
			}
		} else {
			unimplemented!(
				"To support delete operation, create with HyperLogLog::new_with_counters"
			);
		}
	}

	/// Retrieve an estimate of the carginality of the stream.
	pub fn len(&self) -> f64 {
		let v = self.zero;
		if v > 0 {
			let h =
				usize_to_f64(self.m.len()) * (usize_to_f64(self.m.len()) / usize_to_f64(v)).ln();
			if h <= Self::get_threshold(self.p - 4) {
				return h;
			}
		}
		self.ep()
	}

	/// Returns true if empty.
	pub fn is_empty(&self) -> bool {
		self.zero == self.m.len()
	}

	/// Merge another HyperLogLog data structure into `self`.
	///
	/// This is the same as an HLL approximating cardinality of the union of two multisets.
	pub fn union(&mut self, src: &Self) {
		assert_eq!(src.alpha, self.alpha);
		assert_eq!(src.p, self.p);
		assert_eq!(src.m.len(), self.m.len());
		#[cfg(all(
			feature = "packed_simd",
			any(target_arch = "x86", target_arch = "x86_64")
		))]
		{
			assert_eq!(self.m.len() % u8s::lanes(), 0); // TODO: high error rate can trigger this
			assert_eq!(u8s::lanes(), f32s::lanes() * 4);
			assert_eq!(f32s::lanes(), u32s::lanes());
			assert_eq!(u8sq::lanes(), u32s::lanes());
			let mut zero = u8s_sad_out::splat(0);
			let mut sum = f32s::splat(0.0);
			for i in (0..self.m.len()).step_by(u8s::lanes()) {
				unsafe {
					let self_m = u8s::from_slice_unaligned_unchecked(self.m.get_unchecked(i..));
					let src_m = u8s::from_slice_unaligned_unchecked(src.m.get_unchecked(i..));
					let res = self_m.max(src_m);
					res.write_to_slice_unaligned_unchecked(self.m.get_unchecked_mut(i..));
					let count: u8s = u8s::splat(0) - u8s::from_bits(res.eq(u8s::splat(0)));
					let count2 = Sad::<u8s>::sad(count, u8s::splat(0));
					zero += count2;
					for j in 0..4 {
						let x = u8sq::from_slice_unaligned_unchecked(
							self.m.get_unchecked(i + j * u8sq::lanes()..),
						);
						let x: u32s = x.cast();
						let x: f32s = ((u32s::splat(u32::max_value()) - x) << 25 >> 2).into_bits();
						sum += x;
					}
				}
			}
			self.zero = usize::try_from(zero.wrapping_sum()).unwrap();
			self.sum = f64::from(sum.sum());
			// https://github.com/AdamNiederer/faster/issues/37
			// (src.m.simd_iter(faster::u8s(0)),self.m.simd_iter_mut(faster::u8s(0))).zip()
		}
		#[cfg(not(all(
			feature = "packed_simd",
			any(target_arch = "x86", target_arch = "x86_64")
		)))]
		{
			let mut zero = 0;
			let mut sum = 0.0;
			for (to, from) in self.m.iter_mut().zip(src.m.iter()) {
				*to = (*to).max(*from);
				zero += if *to == 0 { 1 } else { 0 };
				sum += f64::from_bits(u64::max_value().wrapping_sub(u64::from(*to)) << 54 >> 2);
			}
			self.zero = zero;
			self.sum = sum;
		}

		if let Some(counters) = &mut self.counters {
			let max_width = 64 - self.p;
			for i in 0..1 << self.p {
				let to = &mut counters[i];
				let from = &src.counters.as_ref().unwrap()[i];
				// From max to 0, merge the max counter
				for j in (0..max_width).rev() {
					let idx = j as usize;
					let to_counter = &mut to[idx];
					let from_counter = &from[idx];
					if *to_counter > 0 || *from_counter > 0 {
						if *to_counter as u16 + *from_counter as u16 > 128 {
							if Self::is_change_power(
								(*to_counter as u16 + *from_counter as u16 - 128) as u8,
							) {
								*to_counter += 1;
							}
						} else {
							*to_counter = *to_counter + *from_counter;
						}
						break;
					}
				}
			}
		}
	}

	/// Intersect another HyperLogLog data structure into `self`.
	///
	/// Note: This is different to an HLL approximating cardinality of the intersection of two multisets.
	pub fn intersect(&mut self, src: &Self) {
		assert_eq!(src.alpha, self.alpha);
		assert_eq!(src.p, self.p);
		assert_eq!(src.m.len(), self.m.len());
		assert_eq!(src.counters.is_some(), self.counters.is_some());
		#[cfg(all(
			feature = "packed_simd",
			any(target_arch = "x86", target_arch = "x86_64")
		))]
		{
			assert_eq!(self.m.len() % u8s::lanes(), 0);
			assert_eq!(u8s::lanes(), f32s::lanes() * 4);
			assert_eq!(f32s::lanes(), u32s::lanes());
			assert_eq!(u8sq::lanes(), u32s::lanes());
			let mut zero = u8s_sad_out::splat(0);
			let mut sum = f32s::splat(0.0);
			for i in (0..self.m.len()).step_by(u8s::lanes()) {
				unsafe {
					let self_m = u8s::from_slice_unaligned_unchecked(self.m.get_unchecked(i..));
					let src_m = u8s::from_slice_unaligned_unchecked(src.m.get_unchecked(i..));
					let res = self_m.min(src_m);
					res.write_to_slice_unaligned_unchecked(self.m.get_unchecked_mut(i..));
					let count: u8s = u8s::splat(0) - u8s::from_bits(res.eq(u8s::splat(0)));
					let count2 = Sad::<u8s>::sad(count, u8s::splat(0));
					zero += count2;
					for j in 0..4 {
						let x = u8sq::from_slice_unaligned_unchecked(
							self.m.get_unchecked(i + j * u8sq::lanes()..),
						);
						let x: u32s = x.cast();
						let x: f32s = ((u32s::splat(u32::max_value()) - x) << 25 >> 2).into_bits();
						sum += x;
					}
				}
			}
			self.zero = usize::try_from(zero.wrapping_sum()).unwrap();
			self.sum = f64::from(sum.sum());
		}
		#[cfg(not(all(
			feature = "packed_simd",
			any(target_arch = "x86", target_arch = "x86_64")
		)))]
		{
			let mut zero = 0;
			let mut sum = 0.0;
			for (to, from) in self.m.iter_mut().zip(src.m.iter()) {
				*to = (*to).min(*from);
				zero += if *to == 0 { 1 } else { 0 };
				sum += f64::from_bits(u64::max_value().wrapping_sub(u64::from(*to)) << 54 >> 2);
			}
			self.zero = zero;
			self.sum = sum;
		}

		if let Some(counters) = &mut self.counters {
			let max_width = 64 - self.p;
			for i in 0..1 << self.p {
				let to = &mut counters[i];
				let from = &src.counters.as_ref().unwrap()[i];
				// From 0 to max, merge the min counter
				for j in 0..max_width {
					let idx = j as usize;
					let from_counter = &from[idx];
					let to_counter = &mut to[idx];
					if *to_counter > 0 || *from_counter > 0 {
						if *to_counter as u16 + *from_counter as u16 > 128 {
							if Self::is_change_power(
								(*to_counter as u16 + *from_counter as u16 - 128) as u8,
							) {
								*to_counter += 1;
							}
						}
						break;
					}
				}
			}
		}
	}

	/// Clears the `HyperLogLog` data structure, as if it was new.
	pub fn clear(&mut self) {
		let max_width = 64 - self.p;
		self.zero = self.m.len();
		self.sum = usize_to_f64(self.m.len());
		self.m.iter_mut().for_each(|x| {
			*x = 0;
		});
		if let Some(counters) = &mut self.counters {
			counters.iter_mut().for_each(|x| {
				*x = vec![0; max_width as usize].into_boxed_slice();
			});
		}
	}

	fn get_threshold(p: u8) -> f64 {
		TRESHOLD_DATA[p as usize]
	}

	fn get_alpha(p: u8) -> f64 {
		assert!(4 <= p && p <= 16);
		match p {
			4 => 0.673,
			5 => 0.697,
			6 => 0.709,
			_ => 0.7213 / (1.0 + 1.079 / u64_to_f64(1_u64 << p)),
		}
	}

	fn get_rho(w: u64, max_width: u8) -> u8 {
		let rho = max_width - (64 - u8::try_from(w.leading_zeros()).unwrap()) + 1;
		assert!(0 < rho && rho < 65);
		rho
	}

	fn estimate_bias(e: f64, p: u8) -> f64 {
		let bias_vector = BIAS_DATA[(p - 4) as usize];
		let neighbors = Self::get_nearest_neighbors(e, RAW_ESTIMATE_DATA[(p - 4) as usize]);
		assert_eq!(neighbors.len(), 6);
		bias_vector[neighbors].iter().sum::<f64>() / 6.0_f64
	}

	fn get_nearest_neighbors(e: f64, estimate_vector: &[f64]) -> Range<usize> {
		let index = estimate_vector
			.binary_search_by(|a| a.partial_cmp(&e).unwrap_or(Ordering::Equal))
			.unwrap_or_else(identity);

		let mut min = if index > 6 { index - 6 } else { 0 };
		let mut max = cmp::min(index + 6, estimate_vector.len());

		while max - min != 6 {
			let (min_val, max_val) = unsafe {
				(
					*estimate_vector.get_unchecked(min),
					*estimate_vector.get_unchecked(max - 1),
				)
			};
			// assert!(min_val <= e && e <= max_val);
			if 2.0 * e - min_val > max_val {
				min += 1;
			} else {
				max -= 1;
			}
		}

		min..max
	}

	fn ep(&self) -> f64 {
		let e = self.alpha * usize_to_f64(self.m.len() * self.m.len()) / self.sum;
		if e <= usize_to_f64(5 * self.m.len()) {
			e - Self::estimate_bias(e, self.p)
		} else {
			e
		}
	}
}

impl<V: ?Sized> Clone for HyperLogLog<V> {
	fn clone(&self) -> Self {
		Self {
			alpha: self.alpha,
			zero: self.zero,
			sum: self.sum,
			p: self.p,
			m: self.m.clone(),
			counters: self.counters.clone(),
			marker: PhantomData,
		}
	}
}
impl<V: ?Sized> fmt::Debug for HyperLogLog<V>
where
	V: Hash,
{
	fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
		fmt.debug_struct("HyperLogLog")
			.field("len", &self.len())
			.finish()
	}
}

impl<V: ?Sized> PartialEq for HyperLogLog<V>
where
	V: Hash,
{
	fn eq(&self, other: &Self) -> bool {
		if self.len().eq(&other.len()) {
			if let Some(self_counters) = &self.counters {
				if let Some(other_counters) = &other.counters {
					return other_counters == self_counters;
				} else {
					return false;
				}
			}
		}

		false
	}
}

impl<V: ?Sized> Eq for HyperLogLog<V> where V: Hash {}

impl<V: ?Sized> New for HyperLogLog<V>
where
	V: Hash,
{
	type Config = f64;
	fn new(config: &Self::Config) -> Self {
		Self::new(*config)
	}
}
impl<V: ?Sized> Intersect for HyperLogLog<V>
where
	V: Hash,
{
	fn intersect<'a>(mut iter: impl Iterator<Item = &'a Self>) -> Option<Self>
	where
		Self: Sized + 'a,
	{
		let mut ret = iter.next()?.clone();
		iter.for_each(|x| {
			ret.intersect(x);
		});
		Some(ret)
	}
}
impl<'a, V: ?Sized> UnionAssign<&'a HyperLogLog<V>> for HyperLogLog<V>
where
	V: Hash,
{
	fn union_assign(&mut self, rhs: &'a Self) {
		self.union(rhs)
	}
}
impl<'a, V: ?Sized> ops::AddAssign<&'a V> for HyperLogLog<V>
where
	V: Hash,
{
	fn add_assign(&mut self, rhs: &'a V) {
		self.push(rhs)
	}
}
impl<'a, V: ?Sized> ops::AddAssign<&'a Self> for HyperLogLog<V>
where
	V: Hash,
{
	fn add_assign(&mut self, rhs: &'a Self) {
		self.union(rhs)
	}
}
impl<V: ?Sized> IntersectPlusUnionIsPlus for HyperLogLog<V> {
	const VAL: bool = true;
}

#[cfg(all(
	feature = "packed_simd",
	any(target_arch = "x86", target_arch = "x86_64")
))]
mod simd {
	pub use packed_simd::{self, Cast, FromBits, IntoBits};
	use std::marker::PhantomData;

	#[cfg(target_feature = "avx512bw")] // TODO
	mod simd_types {
		use super::packed_simd;
		pub type u8s = packed_simd::u8x64;
		pub type u8s_sad_out = packed_simd::u64x8;
		pub type f32s = packed_simd::f32x16;
		pub type u32s = packed_simd::u32x16;
		pub type u8sq = packed_simd::u8x16;
	}
	#[cfg(target_feature = "avx2")]
	mod simd_types {
		#![allow(non_camel_case_types)]
		use super::packed_simd;
		pub type u8s = packed_simd::u8x32;
		pub type u8s_sad_out = packed_simd::u64x4;
		pub type f32s = packed_simd::f32x8;
		pub type u32s = packed_simd::u32x8;
		pub type u8sq = packed_simd::u8x8;
	}
	#[cfg(all(not(target_feature = "avx2"), target_feature = "sse2"))]
	mod simd_types {
		#![allow(non_camel_case_types)]
		use super::packed_simd;
		pub type u8s = packed_simd::u8x16;
		pub type u8s_sad_out = packed_simd::u64x2;
		pub type f32s = packed_simd::f32x4;
		pub type u32s = packed_simd::u32x4;
		pub type u8sq = packed_simd::u8x4;
	}
	#[cfg(all(not(target_feature = "avx2"), not(target_feature = "sse2")))]
	mod simd_types {
		#![allow(non_camel_case_types)]
		use super::packed_simd;
		pub type u8s = packed_simd::u8x8;
		pub type u8s_sad_out = u64;
		pub type f32s = packed_simd::f32x2;
		pub type u32s = packed_simd::u32x2;
		pub type u8sq = packed_simd::u8x2;
	}
	pub use self::simd_types::{f32s, u32s, u8s, u8s_sad_out, u8sq};

	pub struct Sad<X>(PhantomData<fn(X)>);
	#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
	mod x86 {
		#[cfg(target_arch = "x86")]
		pub use std::arch::x86::*;
		#[cfg(target_arch = "x86_64")]
		pub use std::arch::x86_64::*;
	}
	// TODO
	// #[cfg(target_feature = "avx512bw")]
	// impl Sad<packed_simd::u8x64> {
	// 	#[inline]
	// 	#[target_feature(enable = "avx512bw")]
	// 	pub unsafe fn sad(a: packed_simd::u8x64, b: packed_simd::u8x64) -> packed_simd::u64x8 {
	// 		use std::mem::transmute;
	// 		packed_simd::Simd(transmute(x86::_mm512_sad_epu8(transmute(a.0), transmute(b.0))))
	// 	}
	// }
	#[cfg(target_feature = "avx2")]
	impl Sad<packed_simd::u8x32> {
		#[inline]
		#[target_feature(enable = "avx2")]
		pub unsafe fn sad(a: packed_simd::u8x32, b: packed_simd::u8x32) -> packed_simd::u64x4 {
			use std::mem::transmute;
			packed_simd::Simd(transmute(x86::_mm256_sad_epu8(
				transmute(a.0),
				transmute(b.0),
			)))
		}
	}
	#[cfg(target_feature = "sse2")]
	impl Sad<packed_simd::u8x16> {
		#[inline]
		#[target_feature(enable = "sse2")]
		pub unsafe fn sad(a: packed_simd::u8x16, b: packed_simd::u8x16) -> packed_simd::u64x2 {
			use std::mem::transmute;
			packed_simd::Simd(transmute(x86::_mm_sad_epu8(transmute(a.0), transmute(b.0))))
		}
	}
	#[cfg(target_feature = "sse,mmx")]
	impl Sad<packed_simd::u8x8> {
		#[inline]
		#[target_feature(enable = "sse,mmx")]
		pub unsafe fn sad(a: packed_simd::u8x8, b: packed_simd::u8x8) -> u64 {
			use std::mem::transmute;
			transmute(x86::_mm_sad_pu8(transmute(a.0), transmute(b.0)))
		}
	}
	#[cfg(not(target_feature = "sse,mmx"))]
	impl Sad<packed_simd::u8x8> {
		#[inline(always)]
		pub unsafe fn sad(a: packed_simd::u8x8, b: packed_simd::u8x8) -> u64 {
			assert_eq!(b, packed_simd::u8x8::splat(0));
			(0..8).map(|i| u64::from(a.extract(i))).sum()
		}
	}
}
#[cfg(all(
	feature = "packed_simd",
	any(target_arch = "x86", target_arch = "x86_64")
))]
use simd::{f32s, u32s, u8s, u8s_sad_out, u8sq, Cast, FromBits, IntoBits, Sad};

#[cfg(test)]
mod test {
	use super::{super::f64_to_usize, HyperLogLog};
	use std::f64;

	#[test]
	fn pow_bithack() {
		// build the float from x, manipulating it to be the mantissa we want.
		// no portability issues in theory https://doc.rust-lang.org/stable/std/primitive.f64.html#method.from_bits
		for x in 0_u8..65 {
			let a = 2.0_f64.powi(-(i32::from(x)));
			let b = f64::from_bits(u64::max_value().wrapping_sub(u64::from(x)) << 54 >> 2);
			let c = f32::from_bits(u32::max_value().wrapping_sub(u32::from(x)) << 25 >> 2);
			assert_eq!(a, b);
			assert_eq!(a, f64::from(c));
		}
	}

	#[test]
	fn hyperloglog_test_simple() {
		let mut hll = HyperLogLog::new(0.00408);
		let keys = ["test1", "test2", "test3", "test2", "test2", "test2"];
		for k in &keys {
			hll.push(k);
		}
		assert!((hll.len().round() - 3.0).abs() < f64::EPSILON);
		assert!(!hll.is_empty());
		hll.clear();
		assert!(hll.is_empty());
		assert!(hll.len() == 0.0);
	}

	#[test]
	fn hyperloglog_test_merge() {
		let mut hll = HyperLogLog::new(0.00408);
		let keys = ["test1", "test2", "test3", "test2", "test2", "test2"];
		for k in &keys {
			hll.push(k);
		}
		assert!((hll.len().round() - 3.0).abs() < f64::EPSILON);

		let mut hll2 = HyperLogLog::new_from(&hll);
		let keys2 = ["test3", "test4", "test4", "test4", "test4", "test1"];
		for k in &keys2 {
			hll2.push(k);
		}
		assert!((hll2.len().round() - 3.0).abs() < f64::EPSILON);

		hll.union(&hll2);
		assert!((hll.len().round() - 4.0).abs() < f64::EPSILON);
	}

	#[test]
	fn push() {
		let actual = 100_000.0;
		let p = 0.05;
		let mut hll = HyperLogLog::new(p);
		for i in 0..f64_to_usize(actual) {
			hll.push(&i);
		}

		// assert_eq!(111013.12482663046, hll.len());

		assert!(hll.len() > (actual - (actual * p * 3.0)));
		assert!(hll.len() < (actual + (actual * p * 3.0)));
	}

	#[test]
	fn union() {
		let actual = 100_0000;
		let p = 0.05;
		let mut hll1 = HyperLogLog::new_with_counters(p);
		for i in 0..actual {
			hll1.push(&i);
		}
		let mut hll2 = HyperLogLog::new_with_counters(p);
		for i in actual..actual * 2 {
			hll2.push(&i);
		}
		hll1.union(&hll2);
	}

	#[test]
	fn compare_with_counters() {
		let mut hll1 = HyperLogLog::new_with_counters(0.00408);
		let mut hll2 = HyperLogLog::new_with_counters(0.00408);
		hll1.push("test");
		hll2.push("test");
		assert_eq!(hll1, hll2);
		hll2.push("test");
		assert_ne!(hll1, hll2);
	}

	#[test]
	fn compare_with_counters_union() {
		let mut hll1 = HyperLogLog::new_with_counters(0.00408);
		let mut hll2 = HyperLogLog::new_with_counters(0.00408);
		hll1.push("test");
		hll2.push("test");
		hll1.union(&hll2);
		let count = hll1.len();
		hll1.delete("test");
		// after first delete, len will not change.
		assert_eq!(count, hll1.len());
		hll1.delete("test");
		// after second delete, len change to 0.
		assert_eq!(0 as f64, hll1.len());
	}

	#[test]
	fn delete() {
		let mut hll = HyperLogLog::new_with_counters(0.00408);
		// push "test" twice
		for _i in 0..2 {
			hll.push("test");
		}
		let count = hll.len();
		hll.delete("test");
		// after first delete, len will not change.
		assert_eq!(count, hll.len());
		hll.delete("test");
		// after second delete, len change to 0.
		assert_eq!(0 as f64, hll.len());
	}
}
