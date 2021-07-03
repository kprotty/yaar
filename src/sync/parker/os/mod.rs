// Copyright (c) 2021 kprotty
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License

mod instant;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as os;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(target_vendor = "apple")]
mod darwin;
#[cfg(target_vendor = "apple")]
use darwin as os;

#[cfg(all(unix, not(any(target_os = "linux", target_vendor = "apple"))))]
mod posix;
#[cfg(all(unix, not(any(target_os = "linux", target_vendor = "apple"))))]
use posix as os;

pub use os::OsParker;
pub use instant::OsInstant;

pub unsafe trait Parker: Default + Sync {
    type Instant: Ord
        + Clone
        + Add<Duration, Output = Self::Instant>
        + Sub<Self::Instant, Output = Duration>;

    fn now() -> Self::Instant;

    fn yield_now(iteration: usize) -> bool;

    fn prepare(self: Pin<&Self>);

    fn park(self: Pin<&Self>, deadline: Option<Self::Instant>) -> bool;

    fn unpark(self: Pin<&Self>);
}
