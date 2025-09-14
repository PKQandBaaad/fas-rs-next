// Copyright 2024-2025, dependabot[bot], reigadegr, shadow3aaa
//
// This file is part of fas-rs.
//
// fas-rs is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version.
//
// fas-rs is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
// details.
//
// You should have received a copy of the GNU General Public License along
// with fas-rs. If not, see <https://www.gnu.org/licenses/>.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use log::warn;
use nix::sched::CpuSet;

use super::IGNORE_MAP;
use crate::file_handler::FileHandler;

#[derive(Debug)]
pub struct Info {
    pub policy: i32,
    path: PathBuf,
    affected_cpus: Vec<usize>,
    pub cur_fas_freq: isize,
    pub freqs: Vec<isize>,
    verify_freq: Option<isize>,
    verify_timer: Instant,
}

impl Info {
    pub fn new<P>(path: P) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref().to_path_buf();
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("Invalid file name")?;
        let policy_str = file_name.get(6..).context("Invalid policy format")?;
        let policy = policy_str
            .parse::<i32>()
            .context("Failed to parse policy")?;

        let freqs_content = fs::read_to_string(path.join("scaling_available_frequencies"))
            .context("Failed to read frequencies")?;
        let mut freqs: Vec<isize> = freqs_content
            .split_whitespace()
            .map(|f| f.parse::<isize>().context("Failed to parse frequency"))
            .collect::<Result<_>>()?;
        freqs.sort_unstable();

        let affected_cpus = fs::read_to_string(path.join("affected_cpus"))
            .context("Failed to read affected_cpus")?
            .split_whitespace()
            .map(|core| {
                core.parse::<usize>()
                    .context("Failed to parse core")
                    .unwrap()
            })
            .collect();

        Ok(Self {
            policy,
            path,
            affected_cpus,
            cur_fas_freq: *freqs.last().context("No frequencies available")?,
            freqs,
            verify_freq: None,
            verify_timer: Instant::now(),
        })
    }

    fn verify_freq(&mut self, write_freq: isize) {
        if self.verify_timer.elapsed() >= Duration::from_secs(3) {
            self.verify_timer = Instant::now();

            if let Some(verify_freq) = self.verify_freq {
                let current_freq = self.read_freq();
                let min_acceptable_freq = self
                    .freqs
                    .iter()
                    .take_while(|freq| **freq <= verify_freq)
                    .last()
                    .copied()
                    .unwrap_or(verify_freq);
                let max_acceptable_freq = self
                    .freqs
                    .iter()
                    .find(|freq| **freq >= verify_freq)
                    .copied()
                    .unwrap_or(verify_freq);
                if !(min_acceptable_freq..=max_acceptable_freq).contains(&current_freq) {
                    warn!(
                        "CPU Policy{}: Frequency control does not meet expectations! Expected: {}-{}, Actual: {}",
                        self.policy, min_acceptable_freq, max_acceptable_freq, current_freq
                    );
                }
            }
        }

        self.verify_freq = Some(write_freq);
    }

    fn ignore_write(&self) -> Result<bool> {
        Ok(IGNORE_MAP
            .get()
            .context("IGNORE_MAP not initialized")?
            .get(&self.policy)
            .context("Policy ignore flag not found")?
            .load(Ordering::Acquire))
    }

    fn critical_policy(&self, top_used_cores: CpuSet) -> bool {
        self.affected_cpus
            .iter()
            .any(|core| top_used_cores.is_set(*core).unwrap())
    }

    pub fn write_freq(
        &mut self,
        top_used_cores: CpuSet,
        freq: isize,
        file_handler: &mut FileHandler,
    ) -> Result<()> {
        let min_freq = *self.freqs.first().context("No frequencies available")?;
        let max_freq = *self.freqs.last().context("No frequencies available")?;

        let adjusted_freq = freq.clamp(min_freq, max_freq);
        self.cur_fas_freq = adjusted_freq;

        if !self.ignore_write()? {
            if self.critical_policy(top_used_cores) {
                self.verify_freq(adjusted_freq);
                let adjusted_freq = adjusted_freq.to_string();
                file_handler.write_with_workround(self.max_freq_path(), &adjusted_freq)?;
                file_handler.write_with_workround(self.min_freq_path(), &adjusted_freq)?;
            } else {
                let adjusted_freq = adjusted_freq.to_string();
                let min_freq = self
                    .freqs
                    .first()
                    .context("No frequencies available")?
                    .to_string();
                file_handler.write_with_workround(self.min_freq_path(), &min_freq)?;
                file_handler.write_with_workround(self.max_freq_path(), &adjusted_freq)?;
            }
        }

        Ok(())
    }

    pub fn reset(&mut self, file_handler: &mut FileHandler) -> Result<()> {
        let min_freq = self
            .freqs
            .first()
            .context("No frequencies available")?
            .to_string();
        let max_freq = self
            .freqs
            .last()
            .context("No frequencies available")?
            .to_string();
        self.verify_freq = None;

        file_handler.write_with_workround(self.max_freq_path(), &max_freq)?;
        file_handler.write_with_workround(self.min_freq_path(), &min_freq)?;
        Ok(())
    }

    pub fn read_freq(&self) -> isize {
        fs::read_to_string(self.path.join("scaling_cur_freq"))
            .context("Failed to read scaling_cur_freq")
            .unwrap()
            .trim()
            .parse::<isize>()
            .context("Failed to parse scaling_cur_freq")
            .unwrap()
    }

    fn max_freq_path(&self) -> PathBuf {
        self.path.join("scaling_max_freq")
    }

    fn min_freq_path(&self) -> PathBuf {
        self.path.join("scaling_min_freq")
    }
}
