//! What a project is made of, read from its own files and nothing else: the
//! packages its manifests declare (Composer, npm, Cargo, pip, Go, Bundler)
//! with the versions its lock files installed, the technologies those
//! reveal (a PHP framework, a Node server, a bundler), and its Docker side:
//! the services of its Compose files and what its Dockerfiles build from.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use serde_yaml_ng::Value;

use super::{compose_files, env_file, has_compose_file, interpolated, ENV_FILES};

#[derive(Debug, Default, Serialize)]
pub struct Stack {
    pub technologies: Vec<Technology>,
    pub services: Vec<ComposeService>,
    pub dockerfiles: Vec<Dockerfile>,
    pub manifests: Vec<Manifest>,
}

/// One technology the project uses, and the file that says so.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Technology {
    pub name: String,
    pub version: Option<String>,
    /// `Language`, `Framework`, `Frontend`, `Server`, `Build`, `Database`,
    /// `Cache`, `Storage`, `Mail`, `Search`, `Testing`, `Tool`.
    pub kind: &'static str,
    pub from: String,
}

/// A manifest and the packages it asks for.
#[derive(Debug, Serialize)]
pub struct Manifest {
    /// `Composer`, `npm`, `Yarn`, `pnpm`, `Bun`, `Cargo`, `pip`, `Go`, `Bundler`.
    pub manager: String,
    pub file: String,
    pub packages: Vec<Package>,
}

#[derive(Debug, Serialize)]
pub struct Package {
    pub name: String,
    /// What the manifest asks for: `^7.3`.
    pub asked: Option<String>,
    /// What the lock file installed: `7.3.4`.
    pub installed: Option<String>,
    pub dev: bool,
}

/// A service of the Compose files, merged as Compose merges them.
#[derive(Debug, Default, Serialize)]
pub struct ComposeService {
    pub name: String,
    pub image: Option<String>,
    /// What the image is, when it is a known one: `nginx`, `MySQL`.
    pub technology: Option<String>,
    pub kind: Option<&'static str>,
    pub version: Option<String>,
    /// Its build context and Dockerfile, when it is built from the project.
    pub build: Option<String>,
    /// The stage of that Dockerfile it is built to, when it names one.
    pub target: Option<String>,
    /// `8100 → 80`, `443/udp`.
    pub ports: Vec<String>,
    pub depends_on: Vec<String>,
    pub profiles: Vec<String>,
    pub networks: Vec<String>,
}

/// A Dockerfile and the images it builds from, stage after stage.
#[derive(Debug, Serialize)]
pub struct Dockerfile {
    pub file: String,
    pub bases: Vec<String>,
    /// Its stages in order, each with the image it comes down to: a stage
    /// built on an earlier one has that one's image.
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stage {
    pub name: Option<String>,
    pub image: String,
}

/// (package, technology, kind): what a Composer package reveals.
const COMPOSER: &[(&str, &str, &str)] = &[
    ("symfony/framework-bundle", "Symfony", "Framework"),
    ("laravel/framework", "Laravel", "Framework"),
    ("slim/slim", "Slim", "Framework"),
    ("cakephp/cakephp", "CakePHP", "Framework"),
    ("codeigniter4/framework", "CodeIgniter", "Framework"),
    ("yiisoft/yii2", "Yii", "Framework"),
    ("laminas/laminas-mvc", "Laminas", "Framework"),
    ("drupal/core", "Drupal", "Framework"),
    ("roots/wordpress", "WordPress", "Framework"),
    ("johnpbloch/wordpress", "WordPress", "Framework"),
    ("api-platform/core", "API Platform", "Framework"),
    ("api-platform/symfony", "API Platform", "Framework"),
    ("livewire/livewire", "Livewire", "Frontend"),
    ("doctrine/orm", "Doctrine ORM", "Database"),
    ("doctrine/mongodb-odm", "Doctrine MongoDB ODM", "Database"),
    (
        "doctrine/doctrine-migrations-bundle",
        "Doctrine Migrations",
        "Database",
    ),
    ("twig/twig", "Twig", "Frontend"),
    ("symfony/ux-turbo", "Symfony UX Turbo", "Frontend"),
    (
        "symfony/ux-live-component",
        "Symfony UX Live Components",
        "Frontend",
    ),
    ("symfony/webpack-encore-bundle", "Webpack Encore", "Build"),
    ("symfony/asset-mapper", "AssetMapper", "Build"),
    ("symfony/messenger", "Symfony Messenger", "Server"),
    ("symfony/mailer", "Symfony Mailer", "Mail"),
    ("symfony/security-bundle", "Symfony Security", "Framework"),
    ("easycorp/easyadmin-bundle", "EasyAdmin", "Framework"),
    ("sonata-project/admin-bundle", "Sonata Admin", "Framework"),
    ("league/flysystem", "Flysystem", "Storage"),
    ("stripe/stripe-php", "Stripe", "Tool"),
    ("workerman/workerman", "Workerman", "Server"),
    ("cboden/ratchet", "Ratchet", "Server"),
    ("phpunit/phpunit", "PHPUnit", "Testing"),
    ("pestphp/pest", "Pest", "Testing"),
    ("phpstan/phpstan", "PHPStan", "Tool"),
    ("friendsofphp/php-cs-fixer", "PHP CS Fixer", "Tool"),
];

/// What an npm package reveals.
const NPM: &[(&str, &str, &str)] = &[
    ("next", "Next.js", "Framework"),
    ("nuxt", "Nuxt", "Framework"),
    ("@remix-run/react", "Remix", "Framework"),
    ("astro", "Astro", "Framework"),
    ("gatsby", "Gatsby", "Framework"),
    ("@nestjs/core", "NestJS", "Server"),
    ("express", "Express", "Server"),
    ("fastify", "Fastify", "Server"),
    ("koa", "Koa", "Server"),
    ("@hapi/hapi", "hapi", "Server"),
    ("socket.io", "Socket.IO", "Server"),
    ("ws", "ws (WebSocket)", "Server"),
    ("y-websocket", "y-websocket", "Server"),
    ("react", "React", "Frontend"),
    ("react-native", "React Native", "Framework"),
    ("expo", "Expo", "Framework"),
    ("vue", "Vue", "Frontend"),
    ("svelte", "Svelte", "Frontend"),
    ("@angular/core", "Angular", "Framework"),
    ("solid-js", "Solid", "Frontend"),
    ("jquery", "jQuery", "Frontend"),
    ("@hotwired/stimulus", "Stimulus", "Frontend"),
    ("@hotwired/turbo", "Turbo", "Frontend"),
    ("alpinejs", "Alpine.js", "Frontend"),
    ("three", "three.js", "Frontend"),
    ("bootstrap", "Bootstrap", "Frontend"),
    ("tailwindcss", "Tailwind CSS", "Frontend"),
    ("sass", "Sass", "Build"),
    ("typescript", "TypeScript", "Language"),
    ("vite", "Vite", "Build"),
    ("webpack", "webpack", "Build"),
    ("@symfony/webpack-encore", "Webpack Encore", "Build"),
    ("esbuild", "esbuild", "Build"),
    ("rollup", "Rollup", "Build"),
    ("parcel", "Parcel", "Build"),
    ("electron", "Electron", "Framework"),
    ("@tauri-apps/api", "Tauri", "Framework"),
    ("prisma", "Prisma", "Database"),
    ("mongoose", "Mongoose", "Database"),
    ("jest", "Jest", "Testing"),
    ("vitest", "Vitest", "Testing"),
    ("@playwright/test", "Playwright", "Testing"),
    ("cypress", "Cypress", "Testing"),
    ("eslint", "ESLint", "Tool"),
    ("prettier", "Prettier", "Tool"),
];

const CARGO: &[(&str, &str, &str)] = &[
    ("tauri", "Tauri", "Framework"),
    ("axum", "Axum", "Server"),
    ("actix-web", "Actix Web", "Server"),
    ("rocket", "Rocket", "Server"),
    ("warp", "warp", "Server"),
    ("tokio", "Tokio", "Server"),
    ("iroh", "iroh", "Server"),
    ("sqlx", "SQLx", "Database"),
    ("diesel", "Diesel", "Database"),
    ("uniffi", "UniFFI", "Tool"),
];

const PYTHON: &[(&str, &str, &str)] = &[
    ("django", "Django", "Framework"),
    ("flask", "Flask", "Framework"),
    ("fastapi", "FastAPI", "Framework"),
    ("uvicorn", "Uvicorn", "Server"),
    ("gunicorn", "Gunicorn", "Server"),
    ("celery", "Celery", "Server"),
    ("sqlalchemy", "SQLAlchemy", "Database"),
    ("pytest", "pytest", "Testing"),
];

const GO: &[(&str, &str, &str)] = &[
    ("github.com/gin-gonic/gin", "Gin", "Framework"),
    ("github.com/labstack/echo", "Echo", "Framework"),
    ("github.com/gofiber/fiber", "Fiber", "Framework"),
    ("gorm.io/gorm", "GORM", "Database"),
];

const RUBY: &[(&str, &str, &str)] = &[
    ("rails", "Ruby on Rails", "Framework"),
    ("sinatra", "Sinatra", "Framework"),
    ("puma", "Puma", "Server"),
    ("sidekiq", "Sidekiq", "Server"),
    ("rspec", "RSpec", "Testing"),
];

/// (part of the image's name, technology, kind): what a Docker image is.
/// The first that matches wins: the more specific ones come first.
const IMAGES: &[(&str, &str, &str)] = &[
    ("phpmyadmin", "phpMyAdmin", "Tool"),
    ("adminer", "Adminer", "Tool"),
    ("redis-commander", "Redis Commander", "Tool"),
    ("stripe-cli", "Stripe CLI", "Tool"),
    ("nginx", "nginx", "Server"),
    ("httpd", "Apache", "Server"),
    ("apache", "Apache", "Server"),
    ("caddy", "Caddy", "Server"),
    ("traefik", "Traefik", "Server"),
    ("varnish", "Varnish", "Cache"),
    ("haproxy", "HAProxy", "Server"),
    ("frankenphp", "FrankenPHP", "Server"),
    ("mariadb", "MariaDB", "Database"),
    ("mysql", "MySQL", "Database"),
    ("postgis", "PostGIS", "Database"),
    ("postgres", "PostgreSQL", "Database"),
    ("mongo", "MongoDB", "Database"),
    ("clickhouse", "ClickHouse", "Database"),
    ("valkey", "Valkey", "Cache"),
    ("redis", "Redis", "Cache"),
    ("memcached", "Memcached", "Cache"),
    ("rabbitmq", "RabbitMQ", "Server"),
    ("kafka", "Kafka", "Server"),
    ("mercure", "Mercure", "Server"),
    ("minio", "MinIO", "Storage"),
    ("sftpgo", "SFTPGo", "Storage"),
    ("maildev", "MailDev", "Mail"),
    ("mailpit", "Mailpit", "Mail"),
    ("mailhog", "MailHog", "Mail"),
    ("typesense", "Typesense", "Search"),
    ("meilisearch", "Meilisearch", "Search"),
    ("elasticsearch", "Elasticsearch", "Search"),
    ("opensearch", "OpenSearch", "Search"),
    ("selenium", "Selenium", "Testing"),
    ("composer", "Composer", "Tool"),
    // Before the languages: "ubuntu" has "bun" in it.
    ("alpine", "Alpine Linux", "Tool"),
    ("debian", "Debian", "Tool"),
    ("ubuntu", "Ubuntu", "Tool"),
    ("php", "PHP", "Language"),
    ("node", "Node.js", "Language"),
    ("bun", "Bun", "Language"),
    ("deno", "Deno", "Language"),
    ("python", "Python", "Language"),
    ("ruby", "Ruby", "Language"),
    ("golang", "Go", "Language"),
    ("rust", "Rust", "Language"),
    ("openjdk", "Java", "Language"),
    ("eclipse-temurin", "Java", "Language"),
];

/// The folders manifests are not looked for in.
const SKIPPED: [&str; 12] = [
    "node_modules",
    "vendor",
    "var",
    "target",
    "dist",
    "build",
    "public",
    "tests",
    "docs",
    "translations",
    "templates",
    "migrations",
];

/// What the project of `folder` is made of. Files that cannot be read or
/// understood are passed over: this describes, it does not validate.
pub fn detect(folder: &Path) -> Stack {
    let mut stack = Stack::default();

    // The project's own folder, then the folders right under it (a
    // frontend/, an app/, the crates of a workspace).
    let mut folders = vec![(folder.to_path_buf(), String::new())];
    if let Ok(entries) = std::fs::read_dir(folder) {
        let mut inside: Vec<_> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.') && !SKIPPED.contains(&name.as_str()))
            .collect();
        inside.sort();
        for name in inside.into_iter().take(40) {
            folders.push((folder.join(&name), format!("{name}/")));
        }
    }
    for (path, prefix) in &folders {
        composer(path, prefix, &mut stack);
        npm(path, prefix, &mut stack);
        cargo(path, prefix, &mut stack);
        python(path, prefix, &mut stack);
        go(path, prefix, &mut stack);
        ruby(path, prefix, &mut stack);
    }
    compose(folder, &mut stack);
    for (path, prefix) in &folders {
        for name in ["Dockerfile", "Containerfile"] {
            dockerfile(
                folder,
                &path.join(name),
                &format!("{prefix}{name}"),
                &mut stack,
            );
        }
    }

    // Said once each: the first file that reveals it, with a version when
    // one of them knows it.
    let mut technologies: Vec<Technology> = Vec::new();
    for technology in std::mem::take(&mut stack.technologies) {
        match technologies
            .iter_mut()
            .find(|known| known.name == technology.name)
        {
            Some(known) => {
                // An exact version (an image's 8.4) says more than what a
                // manifest asks for (>=8.2).
                let exact = |version: &Option<String>| {
                    version
                        .as_deref()
                        .is_some_and(|version| version.starts_with(|c: char| c.is_ascii_digit()))
                };
                if known.version.is_none() || (!exact(&known.version) && exact(&technology.version))
                {
                    known.version = technology.version;
                }
            }
            None => technologies.push(technology),
        }
    }
    stack.technologies = technologies;
    stack
}

fn technology(
    stack: &mut Stack,
    name: &str,
    version: Option<String>,
    kind: &'static str,
    from: &str,
) {
    stack.technologies.push(Technology {
        name: name.to_string(),
        version: version.filter(|version| !version.is_empty()),
        kind,
        from: from.to_string(),
    });
}

/// The technologies a manifest's packages reveal, from a table.
fn revealed(
    stack: &mut Stack,
    packages: &[Package],
    table: &[(&str, &'static str, &'static str)],
    from: &str,
) {
    for (package, name, kind) in table {
        if let Some(found) = packages.iter().find(|known| known.name == *package) {
            let version = found.installed.clone().or_else(|| found.asked.clone());
            technology(stack, name, version, kind, from);
        }
    }
}

fn json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `v7.3.4` as `7.3.4`.
fn plain(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn composer(folder: &Path, prefix: &str, stack: &mut Stack) {
    let Some(manifest) = json(&folder.join("composer.json")) else {
        return;
    };
    let file = format!("{prefix}composer.json");
    let mut installed: HashMap<String, String> = HashMap::new();
    if let Some(lock) = json(&folder.join("composer.lock")) {
        for group in ["packages", "packages-dev"] {
            for package in lock[group].as_array().into_iter().flatten() {
                if let (Some(name), Some(version)) =
                    (package["name"].as_str(), package["version"].as_str())
                {
                    installed.insert(name.to_string(), plain(version));
                }
            }
        }
    }
    let mut packages = Vec::new();
    for (group, dev) in [("require", false), ("require-dev", true)] {
        for (name, asked) in manifest[group].as_object().into_iter().flatten() {
            let asked = asked.as_str().map(str::to_string);
            // The platform, not a package: the language itself.
            if name == "php" {
                technology(stack, "PHP", asked, "Language", &file);
                continue;
            }
            if name.starts_with("ext-") || name.starts_with("lib-") {
                continue;
            }
            packages.push(Package {
                installed: installed.get(name).cloned(),
                name: name.clone(),
                asked,
                dev,
            });
        }
    }
    if !stack.technologies.iter().any(|known| known.name == "PHP") {
        technology(stack, "PHP", None, "Language", &file);
    }
    revealed(stack, &packages, COMPOSER, &file);
    stack.manifests.push(Manifest {
        manager: "Composer".into(),
        file,
        packages,
    });
}

/// The versions a yarn.lock installed: `name@range:` then `version "x"`
/// (Yarn 1) or `version: x` (Yarn 2 and later).
fn yarn_lock(content: &str) -> HashMap<String, String> {
    let mut installed = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    for line in content.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            names = line
                .trim_end_matches(':')
                .split(", ")
                .filter_map(|entry| {
                    let entry = entry.trim().trim_matches('"');
                    // The name, before the last @ that is not the first character.
                    let at = entry.char_indices().skip(1).find(|(_, c)| *c == '@')?.0;
                    Some(entry[..at].to_string())
                })
                .collect();
        } else if let Some(version) = line.trim().strip_prefix("version") {
            let version = version.trim_start_matches(':').trim().trim_matches('"');
            for name in names.drain(..) {
                installed.entry(name).or_insert_with(|| version.to_string());
            }
        }
    }
    installed
}

fn npm(folder: &Path, prefix: &str, stack: &mut Stack) {
    let Some(manifest) = json(&folder.join("package.json")) else {
        return;
    };
    let file = format!("{prefix}package.json");
    let manager = [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "Yarn"),
        ("bun.lockb", "Bun"),
        ("bun.lock", "Bun"),
    ]
    .iter()
    .find(|(lock, _)| folder.join(lock).is_file())
    .map_or("npm", |(_, manager)| manager);

    let lock = json(&folder.join("package-lock.json"));
    let yarn = std::fs::read_to_string(folder.join("yarn.lock"))
        .map(|content| yarn_lock(&content))
        .unwrap_or_default();
    let installed = |name: &str| -> Option<String> {
        // What is installed, then what the lock files say.
        json(&folder.join("node_modules").join(name).join("package.json"))
            .and_then(|package| package["version"].as_str().map(plain))
            .or_else(|| {
                lock.as_ref().and_then(|lock| {
                    lock["packages"][format!("node_modules/{name}")]["version"]
                        .as_str()
                        .or_else(|| lock["dependencies"][name]["version"].as_str())
                        .map(plain)
                })
            })
            .or_else(|| yarn.get(name).cloned())
    };
    let mut packages = Vec::new();
    for (group, dev) in [("dependencies", false), ("devDependencies", true)] {
        for (name, asked) in manifest[group].as_object().into_iter().flatten() {
            packages.push(Package {
                installed: installed(name),
                name: name.clone(),
                asked: asked.as_str().map(str::to_string),
                dev,
            });
        }
    }
    let node = manifest["engines"]["node"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            [".nvmrc", ".node-version"].iter().find_map(|name| {
                std::fs::read_to_string(folder.join(name))
                    .ok()
                    .map(|version| plain(&version))
            })
        });
    technology(stack, "Node.js", node, "Language", &file);
    revealed(stack, &packages, NPM, &file);
    stack.manifests.push(Manifest {
        manager: manager.into(),
        file,
        packages,
    });
}

fn cargo(folder: &Path, prefix: &str, stack: &mut Stack) {
    let Some(manifest) = std::fs::read_to_string(folder.join("Cargo.toml"))
        .ok()
        .and_then(|content| content.parse::<toml::Table>().ok())
    else {
        return;
    };
    let file = format!("{prefix}Cargo.toml");
    // The lock file is the workspace's: beside the manifest, or above it.
    let lock = [folder.join("Cargo.lock"), folder.join("../Cargo.lock")]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|content| content.parse::<toml::Table>().ok());
    let installed = |name: &str| -> Option<String> {
        lock.as_ref()?
            .get("package")?
            .as_array()?
            .iter()
            .find(|package| package.get("name").and_then(|n| n.as_str()) == Some(name))?
            .get("version")?
            .as_str()
            .map(str::to_string)
    };
    let mut packages = Vec::new();
    let mut read = |table: Option<&toml::Value>, dev: bool| {
        for (name, asked) in table.and_then(|t| t.as_table()).into_iter().flatten() {
            let asked = match asked {
                toml::Value::String(version) => Some(version.clone()),
                other => other
                    .get("version")
                    .and_then(|version| version.as_str())
                    .map(str::to_string),
            };
            packages.push(Package {
                installed: installed(name),
                name: name.clone(),
                asked,
                dev,
            });
        }
    };
    read(manifest.get("dependencies"), false);
    read(
        manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies")),
        false,
    );
    read(manifest.get("dev-dependencies"), true);
    read(manifest.get("build-dependencies"), true);
    let rust = ["package", "workspace"].iter().find_map(|section| {
        let section = manifest.get(*section)?;
        let section = section.get("package").unwrap_or(section);
        section
            .get("rust-version")
            .and_then(|version| version.as_str())
            .map(str::to_string)
    });
    technology(stack, "Rust", rust, "Language", &file);
    revealed(stack, &packages, CARGO, &file);
    if !packages.is_empty() {
        stack.manifests.push(Manifest {
            manager: "Cargo".into(),
            file,
            packages,
        });
    }
}

/// `django>=4.2` as (`django`, `>=4.2`).
fn requirement(line: &str) -> Option<(String, Option<String>)> {
    let line = line.split('#').next()?.split(';').next()?.trim();
    if line.is_empty() || line.starts_with('-') {
        return None;
    }
    let end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(line.len());
    let name = line[..end].to_ascii_lowercase();
    let rest = line[end..].trim();
    // Extras, `[standard]`, are not the version.
    let rest = match rest.strip_prefix('[') {
        Some(extras) => extras.split_once(']').map_or("", |(_, after)| after).trim(),
        None => rest,
    };
    (!name.is_empty()).then(|| (name, (!rest.is_empty()).then(|| rest.to_string())))
}

fn python(folder: &Path, prefix: &str, stack: &mut Stack) {
    let mut found: Vec<(String, Vec<Package>)> = Vec::new();
    if let Ok(content) = std::fs::read_to_string(folder.join("requirements.txt")) {
        let packages = content
            .lines()
            .filter_map(requirement)
            .map(|(name, asked)| Package {
                installed: asked
                    .as_deref()
                    .and_then(|asked| asked.strip_prefix("=="))
                    .map(str::to_string),
                name,
                asked,
                dev: false,
            })
            .collect();
        found.push((format!("{prefix}requirements.txt"), packages));
    }
    if let Some(manifest) = std::fs::read_to_string(folder.join("pyproject.toml"))
        .ok()
        .and_then(|content| content.parse::<toml::Table>().ok())
    {
        let mut packages = Vec::new();
        let declared = manifest
            .get("project")
            .and_then(|project| project.get("dependencies"))
            .and_then(|dependencies| dependencies.as_array());
        for line in declared.into_iter().flatten().filter_map(|d| d.as_str()) {
            if let Some((name, asked)) = requirement(line) {
                packages.push(Package {
                    name,
                    asked,
                    installed: None,
                    dev: false,
                });
            }
        }
        let poetry = manifest
            .get("tool")
            .and_then(|tool| tool.get("poetry"))
            .and_then(|poetry| poetry.get("dependencies"))
            .and_then(|dependencies| dependencies.as_table());
        for (name, asked) in poetry.into_iter().flatten() {
            if name == "python" {
                continue;
            }
            packages.push(Package {
                name: name.to_ascii_lowercase(),
                asked: asked.as_str().map(str::to_string),
                installed: None,
                dev: false,
            });
        }
        if !packages.is_empty() {
            found.push((format!("{prefix}pyproject.toml"), packages));
        }
    }
    for (file, packages) in found {
        technology(stack, "Python", None, "Language", &file);
        revealed(stack, &packages, PYTHON, &file);
        stack.manifests.push(Manifest {
            manager: "pip".into(),
            file,
            packages,
        });
    }
}

fn go(folder: &Path, prefix: &str, stack: &mut Stack) {
    let Ok(content) = std::fs::read_to_string(folder.join("go.mod")) else {
        return;
    };
    let file = format!("{prefix}go.mod");
    let mut packages = Vec::new();
    let mut version = None;
    let mut inside = false;
    for line in content.lines() {
        let line = line.split("//").next().unwrap_or_default().trim();
        if let Some(go) = line.strip_prefix("go ") {
            version = Some(go.trim().to_string());
        } else if line == "require (" {
            inside = true;
        } else if line == ")" {
            inside = false;
        } else if let Some(one) = line.strip_prefix("require ").or(inside.then_some(line)) {
            let mut words = one.split_whitespace();
            if let (Some(name), Some(asked)) = (words.next(), words.next()) {
                packages.push(Package {
                    name: name.to_string(),
                    asked: Some(plain(asked)),
                    installed: Some(plain(asked)),
                    dev: false,
                });
            }
        }
    }
    technology(stack, "Go", version, "Language", &file);
    // A module's major version is part of its path: gin and gin/v2 alike.
    for (module, name, kind) in GO {
        if let Some(found) = packages.iter().find(|known| known.name.starts_with(module)) {
            technology(stack, name, found.installed.clone(), kind, &file);
        }
    }
    stack.manifests.push(Manifest {
        manager: "Go".into(),
        file,
        packages,
    });
}

fn ruby(folder: &Path, prefix: &str, stack: &mut Stack) {
    let Ok(content) = std::fs::read_to_string(folder.join("Gemfile")) else {
        return;
    };
    let file = format!("{prefix}Gemfile");
    // `    rails (7.1.3)` in the lock file.
    let lock = std::fs::read_to_string(folder.join("Gemfile.lock")).unwrap_or_default();
    let installed = |name: &str| -> Option<String> {
        lock.lines().find_map(|line| {
            let line = line.strip_prefix("    ")?;
            let (found, version) = line.split_once(" (")?;
            (found == name && !line.starts_with(' '))
                .then(|| version.trim_end_matches(')').to_string())
        })
    };
    let mut packages = Vec::new();
    for line in content.lines() {
        let Some(gem) = line.trim().strip_prefix("gem ") else {
            continue;
        };
        let mut words = gem
            .split(',')
            .map(|word| word.trim().trim_matches(|c| c == '"' || c == '\''));
        let Some(name) = words.next().filter(|name| !name.is_empty()) else {
            continue;
        };
        let asked = words
            .next()
            .filter(|word| word.starts_with(|c: char| c.is_ascii_digit() || "~><=".contains(c)));
        packages.push(Package {
            installed: installed(name),
            name: name.to_string(),
            asked: asked.map(str::to_string),
            dev: false,
        });
    }
    technology(stack, "Ruby", None, "Language", &file);
    revealed(stack, &packages, RUBY, &file);
    stack.manifests.push(Manifest {
        manager: "Bundler".into(),
        file,
        packages,
    });
}

/// What an image is and its version: `mysql:8.4` is (MySQL, Database, 8.4).
/// The registry and the organisation are not looked at, only the name.
fn image(image: &str) -> (Option<(&'static str, &'static str)>, Option<String>) {
    let reference = image.split('@').next().unwrap_or(image);
    let last = reference.rsplit('/').next().unwrap_or(reference);
    let (name, tag) = last.split_once(':').unwrap_or((last, ""));
    let name = name.to_ascii_lowercase();
    let known = IMAGES
        .iter()
        .find(|(part, _, _)| name.contains(part))
        .map(|(_, technology, kind)| (*technology, *kind));
    // `8.3-fpm-alpine` is 8.3; `alpine`, `latest` say nothing.
    let digits: String = tag
        .trim_start_matches('v')
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let version = digits.trim_end_matches('.').to_string();
    (known, (!version.is_empty()).then_some(version))
}

fn names(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Sequence(names)) => names
            .iter()
            .filter_map(|name| name.as_str().map(str::to_string))
            .collect(),
        Some(Value::Mapping(names)) => names
            .keys()
            .filter_map(|name| name.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn compose(folder: &Path, stack: &mut Stack) {
    let mut variables = HashMap::new();
    for name in ENV_FILES {
        if let Ok(content) = std::fs::read_to_string(folder.join(name)) {
            variables.extend(env_file(&content));
        }
    }
    let composed = variables
        .get("COMPOSE_FILE")
        .is_some_and(|files| !files.is_empty())
        || has_compose_file(folder);
    if !composed {
        return;
    }
    let Ok(files) = compose_files(folder, &variables) else {
        return;
    };
    technology(stack, "Docker Compose", None, "Tool", &files.join(", "));
    let mut services: Vec<ComposeService> = Vec::new();
    for file in &files {
        let Some(mut document) = std::fs::read_to_string(folder.join(file))
            .ok()
            .and_then(|content| serde_yaml_ng::from_str::<Value>(&content).ok())
        else {
            continue;
        };
        document.apply_merge().ok();
        let Some(declared) = document.get("services").and_then(Value::as_mapping) else {
            continue;
        };
        for (name, definition) in declared {
            let Some(name) = name.as_str() else { continue };
            let definition = interpolated(definition, &variables);
            let index = match services.iter().position(|known| known.name == name) {
                Some(index) => index,
                None => {
                    services.push(ComposeService {
                        name: name.to_string(),
                        ..ComposeService::default()
                    });
                    services.len() - 1
                }
            };
            let service = &mut services[index];
            if let Some(image) = definition.get("image").and_then(Value::as_str) {
                service.image = Some(image.to_string());
            }
            match definition.get("build") {
                Some(Value::String(context)) => {
                    service.build = Some(
                        Path::new(context)
                            .join("Dockerfile")
                            .to_string_lossy()
                            .trim_start_matches("./")
                            .to_string(),
                    );
                }
                Some(build @ Value::Mapping(_)) => {
                    let context = build.get("context").and_then(Value::as_str).unwrap_or(".");
                    let dockerfile = build
                        .get("dockerfile")
                        .and_then(Value::as_str)
                        .unwrap_or("Dockerfile");
                    service.build = Some(
                        Path::new(context)
                            .join(dockerfile)
                            .to_string_lossy()
                            .trim_start_matches("./")
                            .to_string(),
                    );
                    if let Some(target) = build.get("target").and_then(Value::as_str) {
                        service.target = Some(target.to_string());
                    }
                }
                _ => {}
            }
            for declared in definition
                .get("ports")
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
            {
                for port in super::ports(name, declared).unwrap_or_default() {
                    let said = format!(
                        "{}{}{}",
                        port.host
                            .map(|host| format!("{host} → "))
                            .unwrap_or_default(),
                        port.container,
                        if port.udp { "/udp" } else { "" }
                    );
                    if !service.ports.contains(&said) {
                        service.ports.push(said);
                    }
                }
            }
            for (key, list) in [
                ("depends_on", &mut service.depends_on),
                ("networks", &mut service.networks),
            ] {
                for name in names(definition.get(key)) {
                    if !list.contains(&name) {
                        list.push(name);
                    }
                }
            }
            if definition.get("profiles").is_some() {
                service.profiles = names(definition.get("profiles"));
            }
        }
    }
    for service in &mut services {
        if let Some(reference) = &service.image {
            let (known, version) = image(reference);
            service.version = version.clone();
            if let Some((name, kind)) = known {
                service.technology = Some(name.to_string());
                service.kind = Some(kind);
                technology(
                    stack,
                    name,
                    version,
                    kind,
                    &format!("service {}", service.name),
                );
            }
        }
        // What it is built from, when the project builds it: its Dockerfile,
        // and the manifests beside it (a Node server in a folder of its own).
        if let Some(build) = service.build.clone() {
            let path = folder.join(&build);
            dockerfile(folder, &path, &build, stack);
            let beside = Path::new(&build).parent().filter(|parent| {
                !parent.as_os_str().is_empty() && !parent.is_absolute() && !build.contains("..")
            });
            if let Some(beside) = beside {
                let prefix = format!("{}/", beside.display());
                let known = stack
                    .manifests
                    .iter()
                    .any(|manifest| manifest.file.starts_with(&prefix));
                if !known {
                    let beside = folder.join(beside);
                    composer(&beside, &prefix, stack);
                    npm(&beside, &prefix, stack);
                    python(&beside, &prefix, stack);
                    go(&beside, &prefix, stack);
                }
            }
            if service.technology.is_none() {
                // The stage it is built to: the one it names, else the last.
                let stage = stack
                    .dockerfiles
                    .iter()
                    .find(|known| known.file == build)
                    .and_then(|known| match &service.target {
                        Some(target) => known
                            .stages
                            .iter()
                            .find(|stage| stage.name.as_deref() == Some(target)),
                        None => known.stages.last(),
                    });
                if let Some(stage) = stage {
                    let (known, version) = image(&stage.image);
                    if let Some((name, kind)) = known {
                        service.technology = Some(name.to_string());
                        service.kind = Some(kind);
                        service.version = version;
                    }
                }
            }
        }
    }
    stack.services = services;
}

/// The images a Dockerfile builds from: its FROM lines, with the defaults
/// of its ARGs filled in, the stages that build on an earlier one aside.
fn dockerfile(folder: &Path, path: &Path, file: &str, stack: &mut Stack) {
    if stack.dockerfiles.iter().any(|known| known.file == file) {
        return;
    }
    // Inside the project only: a build context may point anywhere.
    let inside = path
        .canonicalize()
        .ok()
        .zip(folder.canonicalize().ok())
        .is_some_and(|(path, folder)| path.starts_with(folder));
    let Some(content) = inside.then(|| std::fs::read_to_string(path).ok()).flatten() else {
        return;
    };
    let mut arguments: HashMap<String, String> = HashMap::new();
    let mut stages: Vec<Stage> = Vec::new();
    let mut bases: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        let mut words = line.split_whitespace();
        let Some(instruction) = words.next() else {
            continue;
        };
        if instruction.eq_ignore_ascii_case("ARG") {
            if let Some((name, default)) = words.next().and_then(|word| word.split_once('=')) {
                arguments.insert(name.to_string(), default.trim_matches('"').to_string());
            }
        } else if instruction.eq_ignore_ascii_case("FROM") {
            let mut words = words.filter(|word| !word.starts_with("--"));
            let Some(base) = words.next() else { continue };
            let mut base = base.to_string();
            for (name, default) in &arguments {
                base = base
                    .replace(&format!("${{{name}}}"), default)
                    .replace(&format!("${name}"), default);
            }
            let name = words.next().and(words.next()).map(str::to_string);
            // Built on an earlier stage: that stage's image, not a new one.
            let earlier = stages
                .iter()
                .find(|stage| stage.name.as_deref() == Some(base.as_str()))
                .map(|stage| stage.image.clone());
            stages.push(Stage {
                name,
                image: earlier.clone().unwrap_or_else(|| base.clone()),
            });
            if earlier.is_some() || base == "scratch" || bases.contains(&base) {
                continue;
            }
            let (known, version) = image(&base);
            if let Some((name, kind)) = known {
                technology(stack, name, version, kind, file);
            }
            bases.push(base);
        }
    }
    technology(stack, "Docker", None, "Tool", file);
    stack.dockerfiles.push(Dockerfile {
        file: file.to_string(),
        bases,
        stages,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let folder =
            std::env::temp_dir().join(format!("devshare-stack-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&folder).ok();
        for (file, content) in files {
            let path = folder.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        folder
    }

    fn version<'a>(stack: &'a Stack, name: &str) -> Option<&'a str> {
        let found = stack
            .technologies
            .iter()
            .find(|technology| technology.name == name)
            .unwrap_or_else(|| panic!("{name} not found in {:?}", stack.technologies));
        found.version.as_deref()
    }

    #[test]
    fn a_symfony_project_with_its_assets_and_its_containers() {
        let folder = project(
            "symfony",
            &[
                (
                    "composer.json",
                    r#"{"require": {"php": ">=8.3", "ext-intl": "*", "symfony/framework-bundle": "^7.3", "doctrine/orm": "^3.0", "twig/twig": "^3"},
                        "require-dev": {"phpunit/phpunit": "^11"}}"#,
                ),
                (
                    "composer.lock",
                    r#"{"packages": [{"name": "symfony/framework-bundle", "version": "v7.3.4"}, {"name": "doctrine/orm", "version": "3.5.2"}],
                        "packages-dev": [{"name": "phpunit/phpunit", "version": "11.5.0"}]}"#,
                ),
                (
                    "package.json",
                    r#"{"engines": {"node": ">=22"}, "dependencies": {"@hotwired/stimulus": "^3.2"}, "devDependencies": {"@symfony/webpack-encore": "^5", "sass": "^1.80"}}"#,
                ),
                (
                    "yarn.lock",
                    "# yarn lockfile v1\n\n\"@hotwired/stimulus@^3.2\":\n  version \"3.2.2\"\n  resolved \"https://example\"\n\nsass@^1.80, sass@^1.79:\n  version \"1.80.6\"\n",
                ),
                (
                    "docker-compose.yml",
                    "services:\n  web:\n    build:\n      context: .\n      dockerfile: deployments/web/Dockerfile\n      target: dev\n    depends_on: [database]\n    networks: [intranet]\n  proxy:\n    image: nginx:alpine\n    ports:\n      - \"${APP_HTTPS:-0}:443\"\n    depends_on:\n      web:\n        condition: service_started\n  database:\n    image: mysql:8.4\n  cache:\n    image: redis:7.4-alpine\n    profiles: [cache]\n  chat:\n    build: src/Server/Chat\n",
                ),
                (".env", "APP_HTTPS=8503\n"),
                ("src/Server/Chat/Dockerfile", "FROM node:22-alpine\n"),
                (
                    "src/Server/Chat/package.json",
                    r#"{"dependencies": {"ws": "^8.18"}}"#,
                ),
                (
                    "deployments/web/Dockerfile",
                    "ARG PHP_VERSION=8.3\nFROM php:${PHP_VERSION}-fpm-alpine AS base\nFROM base AS dev\nFROM node:22-alpine AS assets\n",
                ),
            ],
        );
        let stack = detect(&folder);

        // The image's exact version says more than what Composer asks for.
        assert_eq!(version(&stack, "PHP"), Some("8.3"));
        assert_eq!(version(&stack, "Symfony"), Some("7.3.4"));
        assert_eq!(version(&stack, "Doctrine ORM"), Some("3.5.2"));
        // Not installed: what the manifest asks for.
        assert_eq!(version(&stack, "Twig"), Some("^3"));
        assert_eq!(version(&stack, "Node.js"), Some("22"));
        assert_eq!(version(&stack, "Stimulus"), Some("3.2.2"));
        assert_eq!(version(&stack, "Sass"), Some("1.80.6"));
        assert_eq!(version(&stack, "Webpack Encore"), Some("^5"));
        assert_eq!(version(&stack, "MySQL"), Some("8.4"));
        assert_eq!(version(&stack, "Redis"), Some("7.4"));
        assert_eq!(version(&stack, "nginx"), None);
        // Said once, though Composer and the Dockerfile both say PHP.
        assert_eq!(
            stack
                .technologies
                .iter()
                .filter(|t| t.name == "PHP")
                .count(),
            1
        );

        let composer = &stack.manifests[0];
        assert_eq!(
            (composer.manager.as_str(), composer.file.as_str()),
            ("Composer", "composer.json")
        );
        // The language and its extensions are not packages.
        let names: Vec<&str> = composer.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "doctrine/orm",
                "symfony/framework-bundle",
                "twig/twig",
                "phpunit/phpunit"
            ]
        );
        assert!(composer.packages[3].dev);
        assert_eq!(stack.manifests[1].manager, "Yarn");

        let service = |name: &str| stack.services.iter().find(|s| s.name == name).unwrap();
        assert_eq!(service("proxy").ports, ["8503 → 443"]);
        assert_eq!(service("proxy").technology.as_deref(), Some("nginx"));
        assert_eq!(service("proxy").depends_on, ["web"]);
        assert_eq!(
            service("web").build.as_deref(),
            Some("deployments/web/Dockerfile")
        );
        assert_eq!(service("web").networks, ["intranet"]);
        // Built to its dev stage, which comes down to the PHP image; the
        // last stage of that Dockerfile is the assets' Node.
        assert_eq!(service("web").technology.as_deref(), Some("PHP"));
        assert_eq!(service("web").version.as_deref(), Some("8.3"));
        // A Node server in a folder of its own: its Dockerfile, its packages.
        assert_eq!(service("chat").technology.as_deref(), Some("Node.js"));
        assert_eq!(version(&stack, "ws (WebSocket)"), Some("^8.18"));
        assert!(stack
            .manifests
            .iter()
            .any(|manifest| manifest.file == "src/Server/Chat/package.json"));
        assert_eq!(service("cache").profiles, ["cache"]);
        // The stage that builds on an earlier one is not an image.
        assert_eq!(
            stack.dockerfiles[0].bases,
            ["php:8.3-fpm-alpine", "node:22-alpine"]
        );

        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_node_server_under_a_folder_and_other_languages() {
        let folder = project(
            "others",
            &[
                (
                    "api/package.json",
                    r#"{"dependencies": {"express": "^4.19", "ws": "^8"}}"#,
                ),
                (
                    "api/package-lock.json",
                    r#"{"packages": {"node_modules/express": {"version": "4.21.1"}}}"#,
                ),
                ("requirements.txt", "# web\nDjango==5.1.2\nuvicorn[standard]>=0.30 ; python_version > '3.8'\n-r other.txt\n"),
                ("go.mod", "module shop\n\ngo 1.23\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.10.0 // indirect\n)\n"),
                ("node_modules/left/package.json", r#"{"dependencies": {"next": "1"}}"#),
            ],
        );
        let stack = detect(&folder);
        assert_eq!(version(&stack, "Express"), Some("4.21.1"));
        assert_eq!(version(&stack, "ws (WebSocket)"), Some("^8"));
        assert_eq!(version(&stack, "Django"), Some("5.1.2"));
        assert_eq!(version(&stack, "Uvicorn"), Some(">=0.30"));
        assert_eq!(version(&stack, "Go"), Some("1.23"));
        assert_eq!(version(&stack, "Gin"), Some("1.10.0"));
        // node_modules is not the project.
        assert!(!stack.technologies.iter().any(|t| t.name == "Next.js"));
        assert!(stack
            .manifests
            .iter()
            .any(|m| m.file == "api/package.json" && m.manager == "npm"));
        assert!(stack.services.is_empty() && stack.dockerfiles.is_empty());

        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn images_by_their_names_and_versions() {
        assert_eq!(
            image("mysql:8.4"),
            (Some(("MySQL", "Database")), Some("8.4".into()))
        );
        assert_eq!(
            image("registry.example:5000/team/postgres:16.4-alpine@sha256:abc"),
            (Some(("PostgreSQL", "Database")), Some("16.4".into()))
        );
        assert_eq!(
            image("minio/minio:latest"),
            (Some(("MinIO", "Storage")), None)
        );
        assert_eq!(
            image("phpmyadmin/phpmyadmin"),
            (Some(("phpMyAdmin", "Tool")), None)
        );
        assert_eq!(image("my-company/thing:v2.1"), (None, Some("2.1".into())));
    }
}
