//! Frontend connection presets, not a model catalog or a compatibility promise.
//! Official sources and verification boundaries live in docs/guides/model-providers.md.
use super::i18n::Language;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Route {
    pub adapter: &'static str,
    pub base: &'static str,
    // A generation dialect does not determine the catalog path or format.
    // The bool selects the native Anthropic catalog (auth, pagination, metadata).
    pub models: Option<(&'static str, bool)>,
}
impl Route {
    pub fn name(self) -> &'static str {
        match self.adapter {
            "anthropic" => "Anthropic Messages",
            "responses" => "OpenAI Responses",
            _ => "OpenAI Chat Completions",
        }
    }
}
const fn route(adapter: &'static str, base: &'static str, models: Option<&'static str>) -> Route {
    Route {
        adapter,
        base,
        models: match models {
            Some(url) => Some((url, false)),
            None => None,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Service {
    pub en: &'static str,
    pub zh: &'static str,
    // First route is the Lattice default, not an implied official endorsement.
    pub routes: &'static [Route],
}
impl Service {
    pub fn label(self, language: Language) -> &'static str {
        match language {
            Language::English => self.en,
            Language::Chinese => self.zh,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Provider {
    pub en: &'static str,
    pub zh: &'static str,
    pub services: &'static [Service],
}
impl Provider {
    pub fn label(self, language: Language) -> &'static str {
        match language {
            Language::English => self.en,
            Language::Chinese => self.zh,
        }
    }
}

// The first three positions remain stable for existing scripted setup clients.
// Regions are explicit choices, never selected from locale or key contents.
pub(super) const PROVIDERS: &[Provider] = &[
    Provider {
        en: "OpenAI",
        zh: "OpenAI",
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            routes: &[
                route(
                    "responses",
                    "https://api.openai.com/v1",
                    Some("https://api.openai.com/v1/models"),
                ),
                route(
                    "openai",
                    "https://api.openai.com/v1",
                    Some("https://api.openai.com/v1/models"),
                ),
            ],
        }],
    },
    Provider {
        en: "Anthropic",
        zh: "Anthropic",
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            routes: &[
                Route {
                    adapter: "anthropic",
                    base: "https://api.anthropic.com",
                    models: Some(("https://api.anthropic.com/v1/models", true)),
                },
                Route {
                    adapter: "openai",
                    base: "https://api.anthropic.com/v1",
                    models: Some(("https://api.anthropic.com/v1/models", true)),
                },
            ],
        }],
    },
    Provider {
        en: "DeepSeek",
        zh: "DeepSeek",
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            routes: &[
                route(
                    "openai",
                    "https://api.deepseek.com",
                    Some("https://api.deepseek.com/models"),
                ),
                route(
                    "anthropic",
                    "https://api.deepseek.com/anthropic",
                    Some("https://api.deepseek.com/models"),
                ),
                route(
                    "responses",
                    "https://api.deepseek.com",
                    Some("https://api.deepseek.com/models"),
                ),
            ],
        }],
    },
    Provider {
        en: "Google Gemini",
        zh: "Google Gemini",
        services: &[Service {
            en: "Gemini Developer API",
            zh: "Gemini 开发者 API",
            routes: &[route(
                "openai",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                Some("https://generativelanguage.googleapis.com/v1beta/openai/models"),
            )],
        }],
    },
    Provider {
        en: "Alibaba Cloud Model Studio / Qwen",
        zh: "阿里云百炼 / 通义千问",
        services: &[
            Service {
                en: "China (Beijing)",
                zh: "中国（北京）",
                routes: &[
                    route(
                        "openai",
                        "https://dashscope.aliyuncs.com/compatible-mode/v1",
                        None,
                    ),
                    route(
                        "anthropic",
                        "https://dashscope.aliyuncs.com/apps/anthropic",
                        None,
                    ),
                    route(
                        "responses",
                        "https://dashscope.aliyuncs.com/compatible-mode/v1",
                        None,
                    ),
                ],
            },
            Service {
                en: "International (Singapore)",
                zh: "国际（新加坡）",
                routes: &[
                    route(
                        "openai",
                        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
                        None,
                    ),
                    route(
                        "anthropic",
                        "https://dashscope-intl.aliyuncs.com/apps/anthropic",
                        None,
                    ),
                    route(
                        "responses",
                        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
                        None,
                    ),
                ],
            },
            Service {
                en: "US (Virginia)",
                zh: "美国（弗吉尼亚）",
                routes: &[
                    route(
                        "openai",
                        "https://dashscope-us.aliyuncs.com/compatible-mode/v1",
                        None,
                    ),
                    route(
                        "anthropic",
                        "https://dashscope-us.aliyuncs.com/apps/anthropic",
                        None,
                    ),
                    route(
                        "responses",
                        "https://dashscope-us.aliyuncs.com/compatible-mode/v1",
                        None,
                    ),
                ],
            },
        ],
    },
    Provider {
        en: "Moonshot / Kimi",
        zh: "月之暗面 / Kimi",
        services: &[
            Service {
                en: "China",
                zh: "中国站",
                routes: &[
                    route(
                        "openai",
                        "https://api.moonshot.cn/v1",
                        Some("https://api.moonshot.cn/v1/models"),
                    ),
                    route(
                        "anthropic",
                        "https://api.moonshot.cn/anthropic",
                        Some("https://api.moonshot.cn/v1/models"),
                    ),
                    route(
                        "responses",
                        "https://api.moonshot.cn/v1",
                        Some("https://api.moonshot.cn/v1/models"),
                    ),
                ],
            },
            Service {
                en: "Global",
                zh: "全球站",
                routes: &[
                    route(
                        "openai",
                        "https://api.moonshot.ai/v1",
                        Some("https://api.moonshot.ai/v1/models"),
                    ),
                    route(
                        "anthropic",
                        "https://api.moonshot.ai/anthropic",
                        Some("https://api.moonshot.ai/v1/models"),
                    ),
                    route(
                        "responses",
                        "https://api.moonshot.ai/v1",
                        Some("https://api.moonshot.ai/v1/models"),
                    ),
                ],
            },
        ],
    },
    Provider {
        en: "Zhipu / Z.AI / GLM",
        zh: "智谱 / Z.AI / GLM",
        services: &[
            Service {
                en: "BigModel (China)",
                zh: "智谱开放平台（中国）",
                // Messages shares account-dependent coding-plan billing, not a standard preset.
                routes: &[
                    route("openai", "https://open.bigmodel.cn/api/paas/v4", None),
                    route("responses", "https://open.bigmodel.cn/api/v1", None),
                ],
            },
            Service {
                en: "Z.AI (Global)",
                zh: "Z.AI（全球）",
                routes: &[route("openai", "https://api.z.ai/api/paas/v4", None)],
            },
        ],
    },
    Provider {
        en: "MiniMax",
        zh: "MiniMax",
        services: &[
            Service {
                en: "China",
                zh: "中国站",
                routes: &[
                    route(
                        "anthropic",
                        "https://api.minimax.cn/anthropic",
                        Some("https://api.minimax.cn/v1/models"),
                    ),
                    route(
                        "openai",
                        "https://api.minimax.cn/v1",
                        Some("https://api.minimax.cn/v1/models"),
                    ),
                    route(
                        "responses",
                        "https://api.minimax.cn/v1",
                        Some("https://api.minimax.cn/v1/models"),
                    ),
                ],
            },
            Service {
                en: "Global",
                zh: "全球站",
                routes: &[
                    route(
                        "anthropic",
                        "https://api.minimax.io/anthropic",
                        Some("https://api.minimax.io/v1/models"),
                    ),
                    route(
                        "openai",
                        "https://api.minimax.io/v1",
                        Some("https://api.minimax.io/v1/models"),
                    ),
                    route(
                        "responses",
                        "https://api.minimax.io/v1",
                        Some("https://api.minimax.io/v1/models"),
                    ),
                ],
            },
        ],
    },
    Provider {
        en: "Volcengine Ark",
        zh: "火山方舟",
        services: &[Service {
            en: "Beijing — standard API",
            zh: "北京 — 标准 API",
            routes: &[
                route(
                    "responses",
                    "https://ark.cn-beijing.volces.com/api/v3",
                    None,
                ),
                route("openai", "https://ark.cn-beijing.volces.com/api/v3", None),
                route(
                    "anthropic",
                    "https://ark.cn-beijing.volces.com/api/compatible",
                    None,
                ),
            ],
        }],
    },
    Provider {
        en: "SiliconFlow",
        zh: "硅基流动 / SiliconFlow",
        services: &[
            Service {
                en: "China",
                zh: "中国站",
                routes: &[
                    route(
                        "openai",
                        "https://api.siliconflow.cn/v1",
                        Some("https://api.siliconflow.cn/v1/models"),
                    ),
                    route(
                        "anthropic",
                        "https://api.siliconflow.cn",
                        Some("https://api.siliconflow.cn/v1/models"),
                    ),
                ],
            },
            Service {
                en: "Global",
                zh: "全球站",
                routes: &[
                    route(
                        "openai",
                        "https://api.siliconflow.com/v1",
                        Some("https://api.siliconflow.com/v1/models"),
                    ),
                    route(
                        "anthropic",
                        "https://api.siliconflow.com",
                        Some("https://api.siliconflow.com/v1/models"),
                    ),
                ],
            },
        ],
    },
    Provider {
        en: "OpenRouter",
        zh: "OpenRouter",
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            routes: &[
                route(
                    "openai",
                    "https://openrouter.ai/api/v1",
                    Some("https://openrouter.ai/api/v1/models"),
                ),
                route(
                    "anthropic",
                    "https://openrouter.ai/api",
                    Some("https://openrouter.ai/api/v1/models"),
                ),
                route(
                    "responses",
                    "https://openrouter.ai/api/v1",
                    Some("https://openrouter.ai/api/v1/models"),
                ),
            ],
        }],
    },
    Provider {
        en: "xAI",
        zh: "xAI",
        services: &[
            Service {
                en: "Global",
                zh: "全球入口",
                routes: &[
                    route(
                        "responses",
                        "https://api.x.ai/v1",
                        Some("https://api.x.ai/v1/models"),
                    ),
                    route(
                        "openai",
                        "https://api.x.ai/v1",
                        Some("https://api.x.ai/v1/models"),
                    ),
                ],
            },
            Service {
                en: "US regional endpoint",
                zh: "美国区域入口",
                routes: &[route(
                    "responses",
                    "https://us.api.x.ai/v1",
                    Some("https://us.api.x.ai/v1/models"),
                )],
            },
        ],
    },
    Provider {
        en: "Mistral",
        zh: "Mistral",
        services: &[
            Service {
                en: "Standard endpoint",
                zh: "标准入口",
                routes: &[route(
                    "openai",
                    "https://api.mistral.ai/v1",
                    Some("https://api.mistral.ai/v1/models"),
                )],
            },
            Service {
                en: "EU regional endpoint",
                zh: "欧盟区域入口",
                routes: &[route(
                    "openai",
                    "https://api.eu.mistral.ai/v1",
                    Some("https://api.eu.mistral.ai/v1/models"),
                )],
            },
            Service {
                en: "US regional endpoint",
                zh: "美国区域入口",
                routes: &[route(
                    "openai",
                    "https://api.us.mistral.ai/v1",
                    Some("https://api.us.mistral.ai/v1/models"),
                )],
            },
        ],
    },
    Provider {
        en: "Groq",
        zh: "Groq",
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            routes: &[
                route(
                    "openai",
                    "https://api.groq.com/openai/v1",
                    Some("https://api.groq.com/openai/v1/models"),
                ),
                route(
                    "responses",
                    "https://api.groq.com/openai/v1",
                    Some("https://api.groq.com/openai/v1/models"),
                ),
            ],
        }],
    },
    Provider {
        en: "Together AI",
        zh: "Together AI",
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            routes: &[route(
                "openai",
                "https://api.together.ai/v1",
                Some("https://api.together.ai/v1/models"),
            )],
        }],
    },
];

pub(super) fn selected_route(spec: &Value) -> Option<&'static Route> {
    // Compare URL identity, but never rewrite the user's saved address.
    let mut url = reqwest::Url::parse(spec["baseUrl"].as_str()?).ok()?;
    if let Some(host) = url
        .host_str()
        .and_then(|host| host.strip_suffix('.'))
        .map(str::to_owned)
    {
        url.set_host(Some(&host)).ok()?;
    }
    let base = url.as_str().trim_end_matches('/');
    let adapter = spec["adapter"].as_str()?;
    PROVIDERS
        .iter()
        .flat_map(|p| p.services)
        .flat_map(|s| s.routes)
        .find(|r| r.adapter == adapter && r.base == base)
}

pub(super) fn manual_models(spec: &Value) -> bool {
    selected_route(spec).is_some_and(|r| r.models.is_none())
}
