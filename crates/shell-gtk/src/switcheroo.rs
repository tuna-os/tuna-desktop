//! The system's second GPU, as gnome-shell sees it through
//! switcheroo-control (`net.hadess.SwitcherooControl`): `HasDualGpu`
//! decides whether the app menu offers a launch on the other graphics
//! card (appMenu.js `_updateGpuItem`), and `GPUs` names the environment
//! that puts a process on the discrete one (`apply_discrete_gpu_env`,
//! src/shell-app.c).
//!
//! The proxy lives on the system bus and follows the service as it
//! comes and goes: with no switcheroo-control there is no dual GPU.

use std::cell::RefCell;
use std::rc::Rc;

use gio::prelude::*;

const NAME: &str = "net.hadess.SwitcherooControl";
const PATH: &str = "/net/hadess/SwitcherooControl";
const INTERFACE: &str = "net.hadess.SwitcherooControl";

/// One entry of the `GPUs` property.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Gpu {
    pub default: bool,
    pub discrete: bool,
    /// `Environment`: alternating variable names and values, when the
    /// service names one.
    pub environment: Option<Vec<String>>,
}

/// The environment of the GPU `apply_discrete_gpu_env` picks: the first
/// discrete one that is not the default, else the first discrete one,
/// else the first that is not the default. `None` when none names an
/// environment.
pub fn discrete_environment(gpus: &[Gpu]) -> Option<Vec<(String, String)>> {
    let first_nondefault_discrete = gpus.iter().find(|gpu| gpu.discrete && !gpu.default);
    let first_discrete = gpus.iter().find(|gpu| gpu.discrete);
    let first_nondefault = gpus.iter().find(|gpu| !gpu.default);
    [first_nondefault_discrete, first_discrete, first_nondefault]
        .into_iter()
        .flatten()
        .find_map(|gpu| gpu.environment.as_ref())
        .map(|environment| {
            environment
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[name, value]| (name.clone(), value.clone()))
                .collect()
        })
}

fn parse_gpus(variant: &glib::Variant) -> Vec<Gpu> {
    variant
        .iter()
        .map(|gpu| {
            let dict = glib::VariantDict::new(Some(&gpu));
            Gpu {
                default: dict
                    .lookup::<bool>("Default")
                    .ok()
                    .flatten()
                    .unwrap_or(false),
                discrete: dict
                    .lookup::<bool>("Discrete")
                    .ok()
                    .flatten()
                    .unwrap_or(false),
                environment: dict.lookup::<Vec<String>>("Environment").ok().flatten(),
            }
        })
        .collect()
}

/// The switcheroo-control proxy, once the bus connects. GDBusProxy keeps
/// the properties current and clears them when the service leaves, so
/// each read sees the service as it is now (an app menu reads them as it
/// opens).
pub struct Switcheroo {
    proxy: RefCell<Option<gio::DBusProxy>>,
}

impl Switcheroo {
    pub fn new() -> Rc<Self> {
        let this = Rc::new(Self {
            proxy: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        gio::DBusProxy::for_bus(
            gio::BusType::System,
            gio::DBusProxyFlags::NONE,
            None,
            NAME,
            PATH,
            INTERFACE,
            gio::Cancellable::NONE,
            move |proxy| {
                if let (Some(this), Ok(proxy)) = (weak.upgrade(), proxy) {
                    *this.proxy.borrow_mut() = Some(proxy);
                }
            },
        );
        this
    }

    /// `HasDualGpu`: false while no service answers.
    pub fn has_dual_gpu(&self) -> bool {
        self.proxy
            .borrow()
            .as_ref()
            .and_then(|proxy| proxy.cached_property("HasDualGpu"))
            .and_then(|value| value.get::<bool>())
            .unwrap_or(false)
    }

    /// The environment that puts a launch on the discrete GPU.
    pub fn discrete_environment(&self) -> Option<Vec<(String, String)>> {
        let proxy = self.proxy.borrow();
        let gpus = proxy.as_ref()?.cached_property("GPUs")?;
        discrete_environment(&parse_gpus(&gpus))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(default: bool, discrete: bool, environment: &[&str]) -> Gpu {
        Gpu {
            default,
            discrete,
            environment: Some(environment.iter().map(|s| (*s).to_owned()).collect()),
        }
    }

    fn env(pairs: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn the_discrete_card_that_is_not_the_default_wins() {
        let gpus = [
            gpu(true, false, &["DRI_PRIME", "0"]),
            gpu(false, true, &["DRI_PRIME", "1"]),
        ];
        assert_eq!(discrete_environment(&gpus), env(&[("DRI_PRIME", "1")]));
    }

    #[test]
    fn a_default_discrete_card_beats_a_non_default_integrated_one() {
        let gpus = [
            gpu(false, false, &["DRI_PRIME", "2"]),
            gpu(true, true, &["DRI_PRIME", "1"]),
        ];
        assert_eq!(discrete_environment(&gpus), env(&[("DRI_PRIME", "1")]));
    }

    #[test]
    fn any_other_card_is_the_last_resort_and_pairs_are_whole() {
        let gpus = [
            gpu(true, false, &["A", "a"]),
            gpu(
                false,
                false,
                &["DRI_PRIME", "1", "__GLX_VENDOR", "nvidia", "stray"],
            ),
        ];
        assert_eq!(
            discrete_environment(&gpus),
            env(&[("DRI_PRIME", "1"), ("__GLX_VENDOR", "nvidia")]),
        );
        assert_eq!(discrete_environment(&[gpu(true, false, &["A", "a"])]), None);
        assert_eq!(discrete_environment(&[]), None);
    }

    #[test]
    fn a_card_without_an_environment_is_skipped_and_an_empty_one_ends_the_search() {
        // The default card names no environment; the next one does.
        let none = Gpu {
            environment: None,
            ..gpu(true, false, &[])
        };
        let gpus = [none, gpu(false, false, &["DRI_PRIME", "1"])];
        assert_eq!(discrete_environment(&gpus), env(&[("DRI_PRIME", "1")]));
        // The card every choice lands on has none: nothing is applied.
        let bare = Gpu {
            environment: None,
            ..gpu(false, true, &[])
        };
        assert_eq!(
            discrete_environment(&[bare, gpu(false, false, &["DRI_PRIME", "1"])]),
            None
        );
        // An empty list is an environment: nothing is set, and no later
        // card is tried.
        let gpus = [
            gpu(false, true, &[]),
            gpu(false, false, &["DRI_PRIME", "1"]),
        ];
        assert_eq!(discrete_environment(&gpus), env(&[]));
    }
}
