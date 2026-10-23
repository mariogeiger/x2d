//! The printer as the server sees it: what its reports mean, in units and
//! names a reader understands, with nothing of how the printer packs them.

use serde::{Deserialize, Serialize};

/// The printer as the server last saw it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Printer {
    /// Whether the server hears the printer now.
    pub online: bool,
    /// When the printer last reported, in Unix seconds; 0 if it never did.
    pub reported_at: i64,
    /// Where the printer stands with its print.
    pub state: JobState,
    /// The step the printer is at, by its code; see [`stage_label`].
    pub stage: Option<i32>,
    /// The print it holds, if any.
    pub job: Option<Job>,
    /// Its nozzles, by id.
    pub nozzles: Vec<Nozzle>,
    /// The heated bed.
    pub bed: Option<Heater>,
    /// The chamber.
    pub chamber: Option<Heater>,
    /// Its AMS units, by id.
    pub ams: Vec<AmsUnit>,
    /// The spools hung outside any AMS, that hold a filament.
    pub external: Vec<Tray>,
    /// The health alerts (HMS) it raises now.
    pub alerts: Vec<Alert>,
    /// The error that ended or holds the print, if any.
    pub error: Option<Alert>,
    /// Its fans.
    pub fans: Fans,
    /// The speed level, 1 to 4; see [`speed_label`].
    pub speed: Option<u8>,
    /// The Wi-Fi signal, in dBm.
    pub wifi_dbm: Option<i32>,
    /// Whether the chamber light is on.
    pub light: Option<bool>,
}

impl Printer {
    /// The nozzle that extrudes now, if the printer says.
    #[must_use]
    pub fn active_nozzle(&self) -> Option<&Nozzle> {
        self.nozzles.iter().find(|nozzle| nozzle.active)
    }

    /// The temperatures of this moment, as a point of history at `time`.
    #[must_use]
    pub fn sample(&self, time: i64) -> Sample {
        Sample {
            time,
            nozzles: self.nozzles.iter().map(|nozzle| nozzle.heater).collect(),
            bed: self.bed,
            chamber: self.chamber,
        }
    }
}

/// Where a printer stands with its print.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Nothing heard yet.
    #[default]
    Unknown,
    /// Nothing to print.
    Idle,
    /// Getting a print ready.
    Preparing,
    /// Printing.
    Printing,
    /// Paused, by someone or by the printer.
    Paused,
    /// The last print ended well.
    Finished,
    /// The last print failed or was stopped.
    Failed,
    /// A state this program does not know, as the printer names it.
    Other(String),
}

impl JobState {
    /// The state a report names `gcode_state`.
    #[must_use]
    pub fn from_report(name: &str) -> Self {
        match name {
            "IDLE" => Self::Idle,
            "PREPARE" | "SLICING" => Self::Preparing,
            "RUNNING" => Self::Printing,
            "PAUSE" => Self::Paused,
            "FINISH" => Self::Finished,
            "FAILED" => Self::Failed,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Whether a print is under way, paused or not.
    #[must_use]
    pub fn underway(&self) -> bool {
        matches!(self, Self::Preparing | Self::Printing | Self::Paused)
    }

    /// The state as the page names it.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Unknown => "inconnu".into(),
            Self::Idle => "au repos".into(),
            Self::Preparing => "préparation".into(),
            Self::Printing => "impression".into(),
            Self::Paused => "en pause".into(),
            Self::Finished => "terminée".into(),
            Self::Failed => "échouée".into(),
            Self::Other(name) => name.to_lowercase(),
        }
    }
}

/// The print a printer holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Its name, as the slicer sent it.
    pub name: String,
    /// How far it is, from 0 to 100.
    pub percent: u8,
    /// How long the printer expects it still takes, in minutes.
    pub remaining_minutes: u32,
    /// The layer it prints.
    pub layer: u32,
    /// How many layers it has.
    pub total_layers: u32,
    /// The plate of the project it prints, from 1.
    pub plate: u32,
    /// The path of its plate's picture on the server, once fetched.
    pub thumbnail: Option<String>,
}

/// A heater's temperatures, in °C: where it is and where it heads, a target
/// of 0 meaning off. Written as the pair `[current, target]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "(f32, f32)", into = "(f32, f32)")]
pub struct Heater {
    /// Where it is.
    pub current: f32,
    /// Where it heads; 0 when off.
    pub target: f32,
}

impl From<(f32, f32)> for Heater {
    fn from((current, target): (f32, f32)) -> Self {
        Self { current, target }
    }
}

impl From<Heater> for (f32, f32) {
    fn from(heater: Heater) -> Self {
        (heater.current, heater.target)
    }
}

/// One nozzle and its hotend.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Nozzle {
    /// Its id: on the dual-nozzle Bambu printers, 0 is the right one and 1
    /// the left one.
    pub id: u8,
    /// Its hotend.
    pub heater: Heater,
    /// Whether it is the one extruding.
    pub active: bool,
    /// Its diameter, in mm.
    pub diameter: Option<f32>,
    /// Its kind, as the printer names it.
    pub kind: Option<String>,
    /// The slot whose filament it holds.
    pub loaded: Option<Slot>,
}

impl Nozzle {
    /// The nozzle as the page names it.
    #[must_use]
    pub fn label(&self, of: usize) -> String {
        match (of, self.id) {
            (1, _) => "Buse".into(),
            (2, 0) => "Buse droite".into(),
            (2, 1) => "Buse gauche".into(),
            (_, id) => format!("Buse {}", id + 1),
        }
    }
}

/// Where a spool sits: an AMS unit and its tray, or an external holder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    /// The AMS unit; 254 and 255 are the external holders.
    pub unit: u8,
    /// The tray in the unit, from 0.
    pub tray: u8,
}

impl Slot {
    /// Whether this is an external holder rather than an AMS tray.
    #[must_use]
    pub fn external(self) -> bool {
        self.unit >= 254
    }

    /// The slot as the printer's screen names it: `A2` for the second tray
    /// of the first AMS, `HT-A` for the first AMS HT, `Ext.` outside.
    #[must_use]
    pub fn label(self) -> String {
        match self.unit {
            254.. => "Ext.".into(),
            128.. => format!("HT-{}", unit_letter(self.unit - 128)),
            unit => format!("{}{}", unit_letter(unit), self.tray + 1),
        }
    }
}

fn unit_letter(index: u8) -> char {
    char::from(b'A' + index % 26)
}

/// An AMS unit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AmsUnit {
    /// Its id: 0 to 3 for the AMS, from 128 for the AMS HT.
    pub id: u8,
    /// Its relative humidity, in %.
    pub humidity: Option<u8>,
    /// Its temperature, in °C.
    pub temperature: Option<f32>,
    /// How long it still dries, in minutes, while it does.
    pub drying_minutes: Option<u32>,
    /// Its trays, filled or not.
    pub trays: Vec<Tray>,
}

impl AmsUnit {
    /// The unit as the printer's screen names it.
    #[must_use]
    pub fn label(&self) -> String {
        match self.id {
            128.. => format!("AMS HT {}", unit_letter(self.id - 128)),
            id => format!("AMS {}", unit_letter(id)),
        }
    }
}

/// A spool's place and what it holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tray {
    /// Where it sits.
    pub slot: Slot,
    /// Its material, such as `PLA`; none when the tray is empty.
    pub material: Option<String>,
    /// Its product name, such as `PLA Basic`.
    pub name: Option<String>,
    /// Its colours, as `RRGGBB`; several for a multicolour filament.
    pub colors: Vec<String>,
    /// How much is left, in %, when the printer knows.
    pub remain: Option<u8>,
}

/// A health alert or an error, by its code, with what it means.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    /// Its code, as Bambu Lab writes it: `0300_2E00_0003_0001`, or
    /// `0500_4024` for an error.
    pub code: String,
    /// What it means, in French, once known.
    pub text: Option<String>,
}

/// A printer's fans, in % of their full speed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fans {
    /// The part cooling fan.
    pub part: Option<u8>,
    /// The auxiliary fan.
    pub aux: Option<u8>,
    /// The chamber fan.
    pub chamber: Option<u8>,
    /// The hotend's heatbreak fan.
    pub heatbreak: Option<u8>,
}

/// The temperatures at one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// When, in Unix seconds.
    #[serde(rename = "t")]
    pub time: i64,
    /// The nozzles, by id.
    #[serde(rename = "n")]
    pub nozzles: Vec<Heater>,
    /// The bed.
    #[serde(rename = "b")]
    pub bed: Option<Heater>,
    /// The chamber.
    #[serde(rename = "c")]
    pub chamber: Option<Heater>,
}

/// The speed level `level` as the page names it.
#[must_use]
pub fn speed_label(level: u8) -> Option<&'static str> {
    Some(match level {
        1 => "silencieux",
        2 => "standard",
        3 => "sport",
        4 => "fou",
        _ => return None,
    })
}

/// The step of code `stage` as the page names it, for the codes known.
///
/// Codes 0 to 51 follow those the Bambu Lab printers report as `stg_cur`;
/// this X2D was seen at 2, 13, 47 and 48 while preheating, homing and
/// levelling.
#[must_use]
pub fn stage_label(stage: i32) -> Option<&'static str> {
    Some(match stage {
        0 => "impression",
        1 => "nivellement automatique du plateau",
        2 => "préchauffage du plateau",
        3 => "compensation des vibrations",
        4 => "changement de filament",
        5 => "pause M400",
        6 => "pause : plus de filament",
        7 => "chauffe de la buse",
        8 => "calibration de l'extrusion",
        9 => "scan de la surface du plateau",
        10 => "inspection de la première couche",
        11 => "identification du plateau",
        12 | 18 => "calibration du micro-lidar",
        13 => "mise à l'origine de la tête",
        14 => "nettoyage de la buse",
        15 => "vérification de la température de la buse",
        16 => "pause demandée",
        17 => "pause : capot avant tombé",
        19 => "calibration du débit",
        20 => "pause : température de la buse anormale",
        21 => "pause : température du plateau anormale",
        22 => "déchargement du filament",
        23 => "pause : pas sautés",
        24 => "chargement du filament",
        25 => "calibration du bruit des moteurs",
        26 => "pause : AMS perdu",
        27 => "pause : ventilateur du heatbreak trop lent",
        28 => "pause : régulation de la chambre en défaut",
        29 => "refroidissement de la chambre",
        30 => "pause demandée par le G-code",
        31 => "démonstration du bruit des moteurs",
        32 => "pause : filament enroulé sur la buse",
        33 => "pause : coupe-filament en défaut",
        34 => "pause : défaut de première couche",
        35 => "pause : buse bouchée",
        36 => "contrôle de précision avant calibration",
        37 => "calibration de précision absolue",
        38 => "contrôle de précision après calibration",
        39 => "calibration du décalage des buses",
        40 => "nivellement du plateau à haute température",
        41 => "contrôle de l'attache rapide",
        42 => "contrôle de la porte et du capot",
        43 => "calibration du laser",
        44 => "contrôle du matériau laser",
        45 => "contrôle du montage du laser",
        46 => "contrôle du montage du cutter",
        47 => "nivellement du plateau, phase 1",
        48 => "nivellement du plateau, phase 2",
        49 => "chauffe de la chambre",
        50 => "refroidissement du plateau",
        51 => "impression des lignes de calibration",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_named_as_on_the_screen() {
        assert_eq!(Slot { unit: 0, tray: 1 }.label(), "A2");
        assert_eq!(Slot { unit: 1, tray: 3 }.label(), "B4");
        assert_eq!(Slot { unit: 128, tray: 0 }.label(), "HT-A");
        assert_eq!(Slot { unit: 254, tray: 0 }.label(), "Ext.");
        assert!(Slot { unit: 255, tray: 0 }.external());
    }

    #[test]
    fn a_sample_is_a_few_short_pairs() {
        let printer = Printer {
            nozzles: vec![
                Nozzle { id: 0, heater: Heater { current: 42.0, target: 0.0 }, ..Nozzle::default() },
                Nozzle { id: 1, heater: Heater { current: 220.0, target: 220.0 }, ..Nozzle::default() },
            ],
            bed: Some(Heater { current: 55.0, target: 55.0 }),
            ..Printer::default()
        };
        let text = serde_json::to_string(&printer.sample(1_792_755_255)).unwrap();
        assert_eq!(text, r#"{"t":1792755255,"n":[[42.0,0.0],[220.0,220.0]],"b":[55.0,55.0],"c":null}"#);
        assert_eq!(serde_json::from_str::<Sample>(&text).unwrap(), printer.sample(1_792_755_255));
    }

    #[test]
    fn states_read_from_reports() {
        assert_eq!(JobState::from_report("RUNNING"), JobState::Printing);
        assert!(JobState::from_report("PAUSE").underway());
        assert!(!JobState::from_report("FINISH").underway());
        assert_eq!(JobState::from_report("SOMETHING").label(), "something");
    }

    #[test]
    fn two_nozzles_are_right_and_left() {
        let right = Nozzle { id: 0, ..Nozzle::default() };
        let left = Nozzle { id: 1, ..Nozzle::default() };
        assert_eq!(right.label(2), "Buse droite");
        assert_eq!(left.label(2), "Buse gauche");
        assert_eq!(right.label(1), "Buse");
    }
}
