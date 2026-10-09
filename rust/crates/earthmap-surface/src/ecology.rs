use std::cell::OnceCell;

struct EcologyBox {
    longitude: (f64, f64),
    latitude: (f64, f64),
    edge: f64,
    seed: i64,
}
impl EcologyBox {
    fn outside(&self, longitude: f64, latitude: f64) -> bool {
        let margin = self.edge * 2.0;
        longitude < self.longitude.0 - margin
            || longitude > self.longitude.1 + margin
            || latitude < self.latitude.0 - margin
            || latitude > self.latitude.1 + margin
    }
    fn evaluate(&self, longitude: f64, latitude: f64) -> f64 {
        super::surface_material_textured_smooth_box(
            longitude,
            latitude,
            self.longitude.0,
            self.longitude.1,
            self.latitude.0,
            self.latitude.1,
            self.edge,
            self.seed,
        )
    }
}
pub(super) struct EcologyScoreDefinition {
    areas: &'static [EcologyBox],
    texture: Option<(f64, i64, f64)>,
}
impl EcologyScoreDefinition {
    pub(super) fn evaluate(&self, longitude: f64, latitude: f64) -> f64 {
        let mut score = self.areas[0].evaluate(longitude, latitude);
        for area in &self.areas[1..] {
            score = score.max(area.evaluate(longitude, latitude));
        }
        if let Some((frequency, seed, strength)) = self.texture {
            let texture =
                super::surface_material_ecology_noise(longitude, latitude, frequency, seed);
            score = super::clamp_unit(score + ((texture - 0.5) * strength));
        }
        score
    }
    fn upper_bound(&self, longitude: f64, latitude: f64) -> f64 {
        // Outside all jitter margins, boxes contribute at most 0.10 edge
        // texture. Include the outer texture and floating-point slack. This
        // only rejects comparisons; every value used in arithmetic is exact.
        if longitude.is_finite()
            && latitude.is_finite()
            && self
                .areas
                .iter()
                .all(|area| area.outside(longitude, latitude))
        {
            0.100_001 + self.texture.map_or(0.0, |(_, _, strength)| strength * 0.5)
        } else {
            1.0
        }
    }
}
pub(super) const SAHARA: EcologyScoreDefinition = EcologyScoreDefinition {
    areas: &[
        EcologyBox {
            longitude: (-20.0, 38.0),
            latitude: (15.0, 34.0),
            edge: 5.5,
            seed: 0x3340b42e9c8f1231,
        },
        EcologyBox {
            longitude: (35.0, 60.0),
            latitude: (12.0, 32.0),
            edge: 4.0,
            seed: 0x5b18c62a7f921935,
        },
    ],
    texture: None,
};
pub(super) const SAHEL: EcologyScoreDefinition = EcologyScoreDefinition {
    areas: &[EcologyBox {
        longitude: (-20.0, 45.0),
        latitude: (7.0, 17.5),
        edge: 4.5,
        seed: 0x71a9e2d5065c17a1,
    }],
    texture: None,
};
pub(super) const RAINFOREST: EcologyScoreDefinition = EcologyScoreDefinition {
    areas: &[
        EcologyBox {
            longitude: (-16.0, 10.0),
            latitude: (3.0, 10.0),
            edge: 2.5,
            seed: 0x119b6617db734561,
        },
        EcologyBox {
            longitude: (8.0, 33.0),
            latitude: (-9.0, 7.0),
            edge: 4.0,
            seed: 0x3a86d32fd8e71855,
        },
        EcologyBox {
            longitude: (95.0, 145.0),
            latitude: (-11.0, 20.0),
            edge: 4.5,
            seed: 0x6e8ac59a6f19d72b,
        },
        EcologyBox {
            longitude: (-77.0, -45.0),
            latitude: (-16.0, 7.0),
            edge: 4.5,
            seed: 0x243f6a8885a308d3,
        },
    ],
    texture: None,
};
pub(super) const DRY_SAVANNA: EcologyScoreDefinition = EcologyScoreDefinition {
    areas: &[
        EcologyBox {
            longitude: (-18.0, 42.0),
            latitude: (-35.0, -10.0),
            edge: 5.0,
            seed: 0x5225f1ab3df447c9,
        },
        EcologyBox {
            longitude: (24.0, 45.0),
            latitude: (-8.0, 12.0),
            edge: 4.0,
            seed: 0x21cf64acb1a77e15,
        },
        EcologyBox {
            longitude: (-80.0, -36.0),
            latitude: (-34.0, -8.0),
            edge: 4.5,
            seed: 0x789f2bc3d49b7011,
        },
        EcologyBox {
            longitude: (110.0, 155.0),
            latitude: (-38.0, -12.0),
            edge: 5.0,
            seed: 0x14f9e6b7556303f1,
        },
    ],
    texture: Some((0.35, 0x1f5b28a9c472d733, 0.18)),
};
pub(super) const MEDITERRANEAN: EcologyScoreDefinition = EcologyScoreDefinition {
    areas: &[
        EcologyBox {
            longitude: (-11.0, 43.0),
            latitude: (31.0, 46.0),
            edge: 4.0,
            seed: 0x68405c2d09d2ec45,
        },
        EcologyBox {
            longitude: (-125.0, -112.0),
            latitude: (30.0, 42.0),
            edge: 3.0,
            seed: 0x63cf8b99e48aa305,
        },
        EcologyBox {
            longitude: (115.0, 147.0),
            latitude: (-39.0, -28.0),
            edge: 3.5,
            seed: 0x0f73e21989b4c351,
        },
    ],
    texture: Some((0.45, 0x7b3e2f64a91c0d11, 0.16)),
};
pub(super) struct SurfaceMaterialClimate {
    longitude: f64,
    latitude: f64,
    scores: [OnceCell<f64>; 5],
    bounds: [OnceCell<f64>; 5],
    patch: OnceCell<f64>,
    fine: OnceCell<f64>,
}
impl SurfaceMaterialClimate {
    pub(super) fn new(longitude: f64, latitude: f64) -> Self {
        Self {
            longitude,
            latitude,
            scores: std::array::from_fn(|_| OnceCell::new()),
            bounds: std::array::from_fn(|_| OnceCell::new()),
            patch: OnceCell::new(),
            fine: OnceCell::new(),
        }
    }
    fn value(&self, index: usize, definition: &EcologyScoreDefinition) -> f64 {
        *self.scores[index].get_or_init(|| definition.evaluate(self.longitude, self.latitude))
    }
    fn at_least(&self, index: usize, definition: &EcologyScoreDefinition, threshold: f64) -> bool {
        match self.scores[index].get() {
            Some(&value) => value >= threshold,
            None => {
                *self.bounds[index]
                    .get_or_init(|| definition.upper_bound(self.longitude, self.latitude))
                    >= threshold
                    && self.value(index, definition) >= threshold
            }
        }
    }
    pub(super) fn patch(&self) -> f64 {
        *self.patch.get_or_init(|| {
            super::surface_material_ecology_noise(
                self.longitude,
                self.latitude,
                2.4,
                0x4165d9e7a1f31c0b,
            )
        })
    }
    pub(super) fn fine(&self) -> f64 {
        *self.fine.get_or_init(|| {
            super::surface_material_ecology_noise(
                self.longitude,
                self.latitude,
                7.5,
                0x9d6c63b5a8e33f21_u64 as i64,
            )
        })
    }
    #[cfg(test)]
    pub(super) fn with_values(longitude: f64, latitude: f64, values: [f64; 7]) -> Self {
        let context = Self::new(longitude, latitude);
        for (cell, value) in context.scores.iter().zip(values) {
            cell.set(value).unwrap();
        }
        context.patch.set(values[5]).unwrap();
        context.fine.set(values[6]).unwrap();
        context
    }
}
impl SurfaceMaterialClimate {
    pub(super) fn sahara(&self) -> f64 {
        self.value(0, &SAHARA)
    }
    pub(super) fn sahara_at_least(&self, threshold: f64) -> bool {
        self.at_least(0, &SAHARA, threshold)
    }
}
impl SurfaceMaterialClimate {
    pub(super) fn sahel(&self) -> f64 {
        self.value(1, &SAHEL)
    }
    pub(super) fn sahel_at_least(&self, threshold: f64) -> bool {
        self.at_least(1, &SAHEL, threshold)
    }
}
impl SurfaceMaterialClimate {
    pub(super) fn rainforest(&self) -> f64 {
        self.value(2, &RAINFOREST)
    }
    pub(super) fn rainforest_at_least(&self, threshold: f64) -> bool {
        self.at_least(2, &RAINFOREST, threshold)
    }
}
impl SurfaceMaterialClimate {
    pub(super) fn dry_savanna(&self) -> f64 {
        self.value(3, &DRY_SAVANNA)
    }
    pub(super) fn dry_savanna_at_least(&self, threshold: f64) -> bool {
        self.at_least(3, &DRY_SAVANNA, threshold)
    }
}
impl SurfaceMaterialClimate {
    pub(super) fn mediterranean_at_least(&self, threshold: f64) -> bool {
        self.at_least(4, &MEDITERRANEAN, threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geographic_bounds_and_lazy_comparisons_preserve_exact_scores() {
        for (index, definition) in [&SAHARA, &SAHEL, &RAINFOREST, &DRY_SAVANNA, &MEDITERRANEAN]
            .into_iter()
            .enumerate()
        {
            let mut points = Vec::new();
            for longitude in (-180..=180).step_by(15) {
                for latitude in (-90..=90).step_by(10) {
                    points.push((longitude as f64 + 0.125, latitude as f64 - 0.125));
                }
            }
            for area in definition.areas {
                for longitude in [
                    area.longitude.0 - 2.0 * area.edge,
                    area.longitude.0,
                    area.longitude.1,
                    area.longitude.1 + 2.0 * area.edge,
                ] {
                    for latitude in [
                        area.latitude.0 - 2.0 * area.edge,
                        area.latitude.0,
                        area.latitude.1,
                        area.latitude.1 + 2.0 * area.edge,
                    ] {
                        for offset in [-1e-9, 0.0, 1e-9] {
                            points.push((longitude + offset, latitude + offset));
                        }
                    }
                }
            }
            points.push((f64::NAN, 0.0));
            for (longitude, latitude) in points {
                let expected = definition.evaluate(longitude, latitude);
                if expected.is_finite() {
                    assert!(expected <= definition.upper_bound(longitude, latitude));
                }
                for threshold in [1.1, 0.5, 0.35, 0.20, 0.18, 0.10, 0.08, 0.0, f64::NAN] {
                    let climate = SurfaceMaterialClimate::new(longitude, latitude);
                    assert_eq!(
                        climate.at_least(index, definition, threshold),
                        expected >= threshold,
                        "index={index} lon={longitude} lat={latitude} threshold={threshold}"
                    );
                    assert_eq!(
                        climate.value(index, definition).to_bits(),
                        expected.to_bits()
                    );
                    assert_eq!(
                        climate.at_least(index, definition, threshold),
                        expected >= threshold
                    );
                }
            }
        }
        let climate = SurfaceMaterialClimate::new(127.0, 37.0);
        assert!(!climate.rainforest_at_least(0.35));
        assert!(climate.scores[2].get().is_none());
    }
}
