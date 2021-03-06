use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::{
    error::{OptionsError, ParameterScanError},
    parameter::range_step::{Numeric, RangeStep},
};

use clap::ArgMatches;

use crate::parameter::tokenize::tokenize;
use crate::parameter::ParameterValue;

use anyhow::{bail, Result};
use clap::Values;
use rust_decimal::Decimal;

/// A command that should be benchmarked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command<'a> {
    /// The command name (without parameter substitution)
    name: Option<&'a str>,

    /// The command that should be executed (without parameter substitution)
    expression: &'a str,

    /// Zero or more parameter values.
    parameters: Vec<(&'a str, ParameterValue)>,
}

impl<'a> Command<'a> {
    pub fn new(name: Option<&'a str>, expression: &'a str) -> Command<'a> {
        Command {
            name,
            expression,
            parameters: Vec::new(),
        }
    }

    pub fn new_parametrized(
        name: Option<&'a str>,
        expression: &'a str,
        parameters: Vec<(&'a str, ParameterValue)>,
    ) -> Command<'a> {
        Command {
            name,
            expression,
            parameters,
        }
    }

    pub fn get_name(&self) -> String {
        self.name.map_or_else(
            || self.get_shell_command(),
            |name| self.replace_parameters_in(name),
        )
    }

    pub fn get_shell_command(&self) -> String {
        self.replace_parameters_in(self.expression)
    }

    pub fn get_parameters(&self) -> &Vec<(&'a str, ParameterValue)> {
        &self.parameters
    }

    fn replace_parameters_in(&self, original: &str) -> String {
        let mut result = String::new();
        let mut replacements = BTreeMap::<String, String>::new();
        for (param_name, param_value) in &self.parameters {
            replacements.insert(
                format!("{{{param_name}}}", param_name = param_name),
                param_value.to_string(),
            );
        }
        let mut remaining = original;
        // Manually replace consecutive occurrences to avoid double-replacing: e.g.,
        //
        //     hyperfine -L foo 'a,{bar}' -L bar 'baz,quux' 'echo {foo} {bar}'
        //
        // should not ever run 'echo baz baz'. See `test_get_shell_command_nonoverlapping`.
        'outer: while let Some(head) = remaining.chars().next() {
            for (k, v) in &replacements {
                if remaining.starts_with(k.as_str()) {
                    result.push_str(v);
                    remaining = &remaining[k.len()..];
                    continue 'outer;
                }
            }
            result.push(head);
            remaining = &remaining[head.len_utf8()..];
        }
        result
    }
}

/// Finds all the strings that appear multiple times in the input iterator, returning them in
/// sorted order. If no string appears more than once, the result is an empty vector.
fn find_duplicates<'a, I: IntoIterator<Item = &'a str>>(i: I) -> Vec<&'a str> {
    let mut counts = BTreeMap::<&'a str, usize>::new();
    for s in i {
        *counts.entry(s).or_default() += 1;
    }
    counts
        .into_iter()
        .filter_map(|(k, n)| if n > 1 { Some(k) } else { None })
        .collect()
}

fn build_parameterized_commands<'a, T: Numeric>(
    param_min: T,
    param_max: T,
    step: T,
    command_names: Vec<&'a str>,
    command_strings: Vec<&'a str>,
    param_name: &'a str,
) -> Result<Vec<Command<'a>>, ParameterScanError> {
    let param_range = RangeStep::new(param_min, param_max, step)?;
    let param_count = param_range.size_hint().1.unwrap();
    let command_name_count = command_names.len();

    // `--command-name` should appear exactly once or exactly B times,
    // where B is the total number of benchmarks.
    if command_name_count > 1 && command_name_count != param_count {
        return Err(ParameterScanError::UnexpectedCommandNameCount(
            command_name_count,
            param_count,
        ));
    }

    let mut i = 0;
    let mut commands = vec![];
    for value in param_range {
        for cmd in &command_strings {
            let name = command_names
                .get(i)
                .or_else(|| command_names.get(0))
                .copied();
            commands.push(Command::new_parametrized(
                name,
                cmd,
                vec![(param_name, ParameterValue::Numeric(value.into()))],
            ));
            i += 1;
        }
    }
    Ok(commands)
}

fn get_parameterized_commands<'a>(
    command_names: Option<Values<'a>>,
    command_strings: Values<'a>,
    mut vals: clap::Values<'a>,
    step: Option<&str>,
) -> Result<Vec<Command<'a>>, ParameterScanError> {
    let command_names = command_names.map_or(vec![], |names| names.collect::<Vec<&str>>());
    let command_strings = command_strings.collect::<Vec<&str>>();
    let param_name = vals.next().unwrap();
    let param_min = vals.next().unwrap();
    let param_max = vals.next().unwrap();

    // attempt to parse as integers
    if let (Ok(param_min), Ok(param_max), Ok(step)) = (
        param_min.parse::<i32>(),
        param_max.parse::<i32>(),
        step.unwrap_or("1").parse::<i32>(),
    ) {
        return build_parameterized_commands(
            param_min,
            param_max,
            step,
            command_names,
            command_strings,
            param_name,
        );
    }

    // try parsing them as decimals
    let param_min = Decimal::from_str(param_min)?;
    let param_max = Decimal::from_str(param_max)?;

    if step.is_none() {
        return Err(ParameterScanError::StepRequired);
    }

    let step = Decimal::from_str(step.unwrap())?;
    build_parameterized_commands(
        param_min,
        param_max,
        step,
        command_names,
        command_strings,
        param_name,
    )
}

pub struct Commands<'a>(Vec<Command<'a>>);

impl<'a> Commands<'a> {
    pub fn from_cli_arguments(matches: &'a ArgMatches) -> Result<Commands> {
        let command_names = matches.values_of("command-name");
        let command_strings = matches.values_of("command").unwrap();

        if let Some(args) = matches.values_of("parameter-scan") {
            let step_size = matches.value_of("parameter-step-size");
            Ok(Self(get_parameterized_commands(
                command_names,
                command_strings,
                args,
                step_size,
            )?))
        } else if let Some(args) = matches.values_of("parameter-list") {
            let command_names = command_names.map_or(vec![], |names| names.collect::<Vec<&str>>());

            let args: Vec<_> = args.collect();
            let param_names_and_values: Vec<(&str, Vec<String>)> = args
                .chunks_exact(2)
                .map(|pair| {
                    let name = pair[0];
                    let list_str = pair[1];
                    (name, tokenize(list_str))
                })
                .collect();
            {
                let duplicates =
                    find_duplicates(param_names_and_values.iter().map(|(name, _)| *name));
                if !duplicates.is_empty() {
                    bail!("Duplicate parameter names: {}", &duplicates.join(", "));
                }
            }
            let command_list = command_strings.collect::<Vec<&str>>();

            let dimensions: Vec<usize> = std::iter::once(command_list.len())
                .chain(
                    param_names_and_values
                        .iter()
                        .map(|(_, values)| values.len()),
                )
                .collect();
            let param_space_size = dimensions.iter().product();
            if param_space_size == 0 {
                return Ok(Self(Vec::new()));
            }

            // `--command-name` should appear exactly once or exactly B times,
            // where B is the total number of benchmarks.
            let command_name_count = command_names.len();
            if command_name_count > 1 && command_name_count != param_space_size {
                return Err(OptionsError::UnexpectedCommandNameCount(
                    command_name_count,
                    param_space_size,
                )
                .into());
            }

            let mut i = 0;
            let mut commands = Vec::with_capacity(param_space_size);
            let mut index = vec![0usize; dimensions.len()];
            'outer: loop {
                let name = command_names
                    .get(i)
                    .or_else(|| command_names.get(0))
                    .copied();
                i += 1;

                let (command_index, params_indices) = index.split_first().unwrap();
                let parameters = param_names_and_values
                    .iter()
                    .zip(params_indices)
                    .map(|((name, values), i)| (*name, ParameterValue::Text(values[*i].clone())))
                    .collect();
                commands.push(Command::new_parametrized(
                    name,
                    command_list[*command_index],
                    parameters,
                ));

                // Increment index, exiting loop on overflow.
                for (i, n) in index.iter_mut().zip(dimensions.iter()) {
                    *i += 1;
                    if *i < *n {
                        continue 'outer;
                    } else {
                        *i = 0;
                    }
                }
                break 'outer;
            }

            Ok(Self(commands))
        } else {
            let command_names = command_names.map_or(vec![], |names| names.collect::<Vec<&str>>());
            if command_names.len() > command_strings.len() {
                return Err(OptionsError::TooManyCommandNames(command_strings.len()).into());
            }

            let command_list = command_strings.collect::<Vec<&str>>();
            let mut commands = Vec::with_capacity(command_list.len());
            for (i, s) in command_list.iter().enumerate() {
                commands.push(Command::new(command_names.get(i).copied(), s));
            }
            Ok(Self(commands))
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command<'a>> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[test]
fn test_get_shell_command_nonoverlapping() {
    let cmd = Command::new_parametrized(
        None,
        "echo {foo} {bar}",
        vec![
            ("foo", ParameterValue::Text("{bar} baz".into())),
            ("bar", ParameterValue::Text("quux".into())),
        ],
    );
    assert_eq!(cmd.get_shell_command(), "echo {bar} baz quux");
}

#[test]
fn test_get_parameterized_command_name() {
    let cmd = Command::new_parametrized(
        Some("name-{bar}-{foo}"),
        "echo {foo} {bar}",
        vec![
            ("foo", ParameterValue::Text("baz".into())),
            ("bar", ParameterValue::Text("quux".into())),
        ],
    );
    assert_eq!(cmd.get_name(), "name-quux-baz");
}

impl<'a> fmt::Display for Command<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get_shell_command())
    }
}

#[test]
fn test_build_commands_cross_product() {
    use crate::app::get_cli_arguments;

    let matches = get_cli_arguments(vec![
        "hyperfine",
        "-L",
        "par1",
        "a,b",
        "-L",
        "par2",
        "z,y",
        "echo {par1} {par2}",
        "printf '%s\n' {par1} {par2}",
    ]);
    let result = Commands::from_cli_arguments(&matches).unwrap().0;

    // Iteration order: command list first, then parameters in listed order (here, "par1" before
    // "par2", which is distinct from their sorted order), with parameter values in listed order.
    let pv = |s: &str| ParameterValue::Text(s.to_string());
    let cmd = |cmd: usize, par1: &str, par2: &str| {
        let expression = ["echo {par1} {par2}", "printf '%s\n' {par1} {par2}"][cmd];
        let params = vec![("par1", pv(par1)), ("par2", pv(par2))];
        Command::new_parametrized(None, expression, params)
    };
    let expected = vec![
        cmd(0, "a", "z"),
        cmd(1, "a", "z"),
        cmd(0, "b", "z"),
        cmd(1, "b", "z"),
        cmd(0, "a", "y"),
        cmd(1, "a", "y"),
        cmd(0, "b", "y"),
        cmd(1, "b", "y"),
    ];
    assert_eq!(result, expected);
}

#[test]
fn test_build_parameter_list_commands() {
    use crate::app::get_cli_arguments;

    let matches = get_cli_arguments(vec![
        "hyperfine",
        "echo {foo}",
        "--parameter-list",
        "foo",
        "1,2",
        "--command-name",
        "name-{foo}",
    ]);
    let commands = Commands::from_cli_arguments(&matches).unwrap().0;
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].get_name(), "name-1");
    assert_eq!(commands[1].get_name(), "name-2");
    assert_eq!(commands[0].get_shell_command(), "echo 1");
    assert_eq!(commands[1].get_shell_command(), "echo 2");
}

#[test]
fn test_build_parameter_range_commands() {
    use crate::app::get_cli_arguments;
    let matches = get_cli_arguments(vec![
        "hyperfine",
        "echo {val}",
        "--parameter-scan",
        "val",
        "1",
        "2",
        "--parameter-step-size",
        "1",
        "--command-name",
        "name-{val}",
    ]);
    let commands = Commands::from_cli_arguments(&matches).unwrap().0;
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].get_name(), "name-1");
    assert_eq!(commands[1].get_name(), "name-2");
    assert_eq!(commands[0].get_shell_command(), "echo 1");
    assert_eq!(commands[1].get_shell_command(), "echo 2");
}

#[test]
fn test_get_parameterized_commands_int() {
    let commands =
        build_parameterized_commands(1i32, 7i32, 3i32, vec![], vec!["echo {val}"], "val").unwrap();
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[2].get_name(), "echo 7");
    assert_eq!(commands[2].get_shell_command(), "echo 7");
}

#[test]
fn test_get_parameterized_commands_decimal() {
    let param_min = Decimal::from_str("0").unwrap();
    let param_max = Decimal::from_str("1").unwrap();
    let step = Decimal::from_str("0.33").unwrap();

    let commands = build_parameterized_commands(
        param_min,
        param_max,
        step,
        vec![],
        vec!["echo {val}"],
        "val",
    )
    .unwrap();
    assert_eq!(commands.len(), 4);
    assert_eq!(commands[3].get_name(), "echo 0.99");
    assert_eq!(commands[3].get_shell_command(), "echo 0.99");
}

#[test]
fn test_get_parameterized_command_names() {
    let commands = build_parameterized_commands(
        1i32,
        3i32,
        1i32,
        vec!["name-{val}"],
        vec!["echo {val}"],
        "val",
    )
    .unwrap();
    assert_eq!(commands.len(), 3);
    let command_names = commands
        .iter()
        .map(|c| c.get_name())
        .collect::<Vec<String>>();
    assert_eq!(command_names, vec!["name-1", "name-2", "name-3"]);
}

#[test]
fn test_get_specified_command_names() {
    let commands = build_parameterized_commands(
        1i32,
        3i32,
        1i32,
        vec!["name-a", "name-b", "name-c"],
        vec!["echo {val}"],
        "val",
    )
    .unwrap();
    assert_eq!(commands.len(), 3);
    let command_names = commands
        .iter()
        .map(|c| c.get_name())
        .collect::<Vec<String>>();
    assert_eq!(command_names, vec!["name-a", "name-b", "name-c"]);
}

#[test]
fn test_different_command_name_count_with_parameters() {
    let result = build_parameterized_commands(
        1i32,
        3i32,
        1i32,
        vec!["name-1", "name-2"],
        vec!["echo {val}"],
        "val",
    );
    assert_eq!(
        format!("{}", result.unwrap_err()),
        "'--command-name' has been specified 2 times. It has to appear exactly once, or exactly 3 times (number of benchmarks)"
    );
}
