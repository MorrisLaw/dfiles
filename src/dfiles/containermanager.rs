use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

use clap::{App, ArgMatches, SubCommand};
use dockworker::{ContainerBuildOptions, Docker};
use dyn_clone;
use serde::Deserialize;
use serde_json::from_str;
use tar::{Builder, Header};
use tempfile::NamedTempFile;

use super::aspects;
use super::config;
use super::docker;
use super::error::{Error, Result};

#[derive(Deserialize, Debug)]
struct BuildOutput {
    stream: String,
}

pub struct ContainerManager {
    name: String,
    tags: Vec<String>,
    container_paths: Vec<String>,
    aspects: Vec<Box<dyn aspects::ContainerAspect>>,
    args: Vec<String>,
}

impl ContainerManager {
    pub fn default_debian(
        name: String,
        tags: Vec<String>,
        container_paths: Vec<String>,
        mut aspects: Vec<Box<dyn aspects::ContainerAspect>>,
        args: Vec<String>,
    ) -> ContainerManager {
        aspects.insert(0, Box::new(Debian {}));
        ContainerManager {
            name: name,
            tags: tags,
            container_paths: container_paths,
            aspects: aspects,
            args: args,
        }
    }

    fn image(&self) -> String {
        self.tags[0].clone()
    }

    fn run(&self, matches: &ArgMatches) -> Result<()> {
        let mut args: Vec<String> = vec!["--rm"].into_iter().map(String::from).collect();

        for aspect in &self.aspects {
            println!("{:}", aspect);
            args.extend(aspect.run_args(Some(&matches))?);
        }

        args.push(self.image().to_string());
        args.extend_from_slice(&self.args);
        docker::run(args);
        Ok(())
    }

    fn build(&self) -> Result<()> {
        let mut tar_file = NamedTempFile::new()?;
        self.generate_archive_impl(&mut tar_file.as_file_mut())?;

        let docker = Docker::connect_with_defaults()?;
        let options = ContainerBuildOptions {
            dockerfile: "Dockerfile".into(),
            t: self.tags.clone(),
            ..ContainerBuildOptions::default()
        };

        let res = docker.build_image(options, tar_file.path())?;
        BufReader::new(res)
            .lines()
            .filter_map(std::result::Result::ok)
            .map(|l| from_str::<BuildOutput>(&l))
            .filter_map(std::result::Result::ok)
            .for_each(|bo: BuildOutput| print!("{}", bo.stream));
        Ok(())
    }

    fn generate_archive_impl(&self, f: &mut std::fs::File) -> Result<()> {
        let mut a = Builder::new(f);

        let mut contents: BTreeMap<u8, String> = BTreeMap::new();
        for aspect in &self.aspects {
            let dockerfile_snippets = aspect.dockerfile_snippets();
            for snippet in dockerfile_snippets {
                contents
                    .entry(snippet.order)
                    .and_modify(|e| {
                        e.push('\n');
                        e.push_str(snippet.content.as_str());
                    })
                    .or_insert(snippet.content);
            }
            for file in aspect.container_files() {
                add_file_to_archive(&mut a, &file.container_path, &file.contents)?;
            }
        }

        let mut dockerfile_contents = String::new();

        for content in contents.values() {
            dockerfile_contents.push_str(content.as_str());
            dockerfile_contents.push('\n');
            dockerfile_contents.push('\n');
        }

        add_file_to_archive(&mut a, "Dockerfile", &dockerfile_contents)?;

        Ok(())
    }

    fn generate_archive(&self) -> Result<()> {
        let mut tar_file = File::create("whatever.tar")?;
        self.generate_archive_impl(&mut tar_file)
    }

    /// Takes configuration options for the dfiles binary and saves them to be loaded at build or
    /// run time.
    ///
    /// dfiles strives to provide a configurable framework for building and running GUI containers.
    /// to achieve this configurability, we allow dynamic Aspects to be loaded from configuration
    /// files. Those configuration files can be hand-written but we also provide a `config`
    /// subcommand.
    ///
    /// ```bash
    /// $ firefox config --mount <hostpath>:<containerpath>
    /// ```
    fn config(&self, matches: &ArgMatches) -> Result<()> {
        let cfg = config::Config::try_from(matches)?;

        let mut profile: Option<&str> = None;
        if matches.occurrences_of("profile") > 0 {
            profile = matches.value_of("profile");
        }

        cfg.save(Some(&self.name), profile)
    }

    fn load_config(&mut self, matches: &ArgMatches) -> Result<()> {
        let mut profile: Option<&str> = None;
        if matches.occurrences_of("profile") > 0 {
            profile = matches.value_of("profile");
        }
        let cfg = config::Config::load(&self.name, profile)?;

        let cli_cfg = config::Config::try_from(matches)?;

        self.aspects
            .extend(cfg.merge(&cli_cfg, false).get_aspects());
        Ok(())
    }

    pub fn execute(&mut self) -> Result<()> {
        let mut run = SubCommand::with_name("run").about("run app in container");
        let mut build = SubCommand::with_name("build").about("build app container");
        let mut config = SubCommand::with_name("config").about("configure app container settings");
        let generate_archive = SubCommand::with_name("generate-archive")
            .about("generate archive used to build container");

        let mut app = App::new(&self.name).version("0.0");

        self.aspects.insert(
            0,
            Box::new(aspects::Profile {
                name: self.name.clone(),
                container_paths: self.container_paths.clone(),
            }),
        );

        for arg in &config::cli_args() {
            run = run.arg(arg);
            config = config.arg(arg);
        }

        let cloned = dyn_clone::clone_box(&self.aspects);
        for aspect in cloned.iter() {
            for arg in aspect.config_args() {
                run = run.arg(arg);
            }
            for arg in aspect.cli_build_args() {
                build = build.arg(arg);
            }
            for arg in aspect.config_args() {
                config = config.arg(arg);
            }
        }

        app = app
            .subcommand(run)
            .subcommand(build)
            .subcommand(config)
            .subcommand(generate_archive);

        let matches = app.get_matches();
        let (subc, subm) = matches.subcommand();

        if let Some(v) = subm {
            self.load_config(&v)?;
        }

        match (subc, subm) {
            ("run", Some(subm)) => self.run(&subm),
            ("build", _) => self.build(),
            ("config", Some(subm)) => self.config(&subm),
            ("generate-archive", _) => self.generate_archive(),
            (_, _) => Ok(println!("{}", matches.usage())),
        }
    }
}

fn add_file_to_archive<W: Write>(b: &mut Builder<W>, name: &str, contents: &str) -> Result<()> {
    let mut header = Header::new_gnu();
    header
        .set_path(name)
        .map_err(|e| Error::FailedToAddFileToArchive { source: e })?;
    header.set_size(contents.len() as u64);
    header.set_cksum();
    b.append(&header, contents.as_bytes())
        .map_err(|e| Error::FailedToAddFileToArchive { source: e })
}

#[derive(Clone)]
struct Debian {}

impl aspects::ContainerAspect for Debian {
    fn name(&self) -> String {
        String::from("Debian")
    }
    fn dockerfile_snippets(&self) -> Vec<aspects::DockerfileSnippet> {
        vec![
            aspects::DockerfileSnippet {
                order: 00,
                content: String::from("FROM debian:buster"),
            },
            aspects::DockerfileSnippet {
                order: 3,
                content: String::from(
                    r#"# Useful language packs
RUN apt-get update && apt-get install -y --no-install-recommends \
  fonts-arphic-bkai00mp \
  fonts-arphic-bsmi00lp \
  fonts-arphic-gbsn00lp \
  fonts-arphic-gbsn00lp \
  \
  && rm -rf /var/lib/apt/lists/* \
  && rm -rf /src/*.deb"#,
                ),
            },
            aspects::DockerfileSnippet {
                order: 2,
                content: String::from(
                    r#"RUN apt-get update && apt-get install -y \
    --no-install-recommends \
    apt-utils \
    apt-transport-https \
    apt \
    bzip2 \
    ca-certificates \
    curl \
    debian-goodies \
    dirmngr \
    gnupg \
    keychain \
    lsb-release \
    locales \
    lsof \
    procps \
  && apt-get purge --autoremove \
  && rm -rf /var/lib/apt/lists/* \
  && rm -rf /src/*.deb "#,
                ),
            },
        ]
    }
}
