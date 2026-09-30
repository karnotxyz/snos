/// Configuration for OS hints and execution parameters.
///
/// This struct controls various aspects of the Starknet OS execution, including
/// debug mode, output verbosity, and data availability mode.
///
/// # Examples
///
/// ```rust
/// use generate_pie::OsHintsConfiguration;
///
/// // Use default configuration
/// let config = OsHintsConfiguration::default();
///
/// // Create custom configuration for debugging
/// let debug_config = OsHintsConfiguration {
///     debug_mode: true,
///     full_output: true,
///     use_kzg_da: false,
///     committed_data_activation_block: None, committed_data_readers: Default::default(), committed_data_witnesses: Vec::new(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OsHintsConfiguration {
    /// Whether to enable debug mode for detailed logging and output.
    pub debug_mode: bool,
    /// Whether to generate full output including intermediate states.
    pub full_output: bool,
    /// Whether to use KZG (Kate-Zaverucha-Goldberg) data availability mode.
    pub use_kzg_da: bool,
    /// Inclusive extension activation height; must match the chain settlement configuration.
    pub committed_data_activation_block: Option<u64>,
    /// Approved adapter storage addresses; empty denies every caller. Bound into the OS configuration.
    pub committed_data_readers: starknet_api::committed_data::CommittedDataReaders,
    /// Private committed_data witnesses keyed by root, publisher and index. Never included in tx calldata.
    #[serde(deserialize_with = "blockifier::execution::syscalls::committed_data::deserialize_witnesses")]
    pub committed_data_witnesses: Vec<blockifier::execution::syscalls::committed_data::CommittedDataWitness>,
}

impl Default for OsHintsConfiguration {
    /// Creates a default configuration with sensible defaults.
    ///
    /// # Returns
    ///
    /// A `OsHintsConfiguration` instance with:
    /// - Debug mode: enabled (for better error reporting)
    /// - Full output: enabled (for local aggregation)
    /// - KZG DA: disabled (selected later by the aggregator)
    fn default() -> Self {
        Self {
            debug_mode: true,
            full_output: true,
            use_kzg_da: false,
            committed_data_activation_block: None,
            committed_data_readers: Default::default(),
            committed_data_witnesses: Vec::new(),
        }
    }
}

impl OsHintsConfiguration {
    pub fn default_with_is_l3(is_l3: bool) -> OsHintsConfiguration {
        if is_l3 {
            Self {
                debug_mode: true,
                full_output: false,
                use_kzg_da: false,
                committed_data_activation_block: None,
                committed_data_readers: Default::default(),
                committed_data_witnesses: Vec::new(),
            }
        } else {
            Self {
                debug_mode: true,
                full_output: true,
                use_kzg_da: false,
                committed_data_activation_block: None,
                committed_data_readers: Default::default(),
                committed_data_witnesses: Vec::new(),
            }
        }
    }
}
