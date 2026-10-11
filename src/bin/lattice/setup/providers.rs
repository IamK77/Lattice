//! Frontend connection presets, not a model catalog or a compatibility promise.
//! Official sources and verification boundaries live in docs/guides/model-providers.md.
use super::i18n::Language;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Service {
    pub en: &'static str,
    pub zh: &'static str,
    pub base: &'static str,
    pub discovery: bool,
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
    pub protocols: &'static [(&'static str, &'static str)],
}
impl Provider {
    pub fn label(self, language: Language) -> &'static str {
        match language {
            Language::English => self.en,
            Language::Chinese => self.zh,
        }
    }
    pub fn base(self, service: usize, adapter: &str) -> &'static str {
        if self.services[service].base == "https://api.deepseek.com" && adapter == "anthropic" {
            "https://api.deepseek.com/anthropic"
        } else if self.services[service].base == "https://api.anthropic.com" && adapter == "openai"
        {
            "https://api.anthropic.com/v1"
        } else {
            self.services[service].base
        }
    }
}

// The first three positions remain stable for existing scripted setup clients.
// Regions are explicit choices, never selected from locale or key contents.
pub(super) const PROVIDERS: &[Provider] = &[
    Provider {
        en: "OpenAI",
        zh: "OpenAI",
        protocols: &[
            ("responses", "OpenAI Responses"),
            ("openai", "OpenAI Chat Completions"),
        ],
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            base: "https://api.openai.com/v1",
            discovery: true,
        }],
    },
    Provider {
        en: "Anthropic",
        zh: "Anthropic",
        protocols: &[
            ("anthropic", "Anthropic Messages"),
            ("openai", "OpenAI Chat Completions"),
        ],
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            base: "https://api.anthropic.com",
            discovery: true,
        }],
    },
    Provider {
        en: "DeepSeek",
        zh: "DeepSeek",
        protocols: &[
            ("openai", "OpenAI Chat Completions"),
            ("anthropic", "Anthropic Messages"),
            ("responses", "OpenAI Responses"),
        ],
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            base: "https://api.deepseek.com",
            discovery: true,
        }],
    },
    Provider {
        en: "Google Gemini",
        zh: "Google Gemini",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[Service {
            en: "Gemini Developer API",
            zh: "Gemini 开发者 API",
            base: "https://generativelanguage.googleapis.com/v1beta/openai",
            discovery: true,
        }],
    },
    Provider {
        en: "Alibaba Cloud Model Studio / Qwen",
        zh: "阿里云百炼 / 通义千问",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[
            Service {
                en: "China (Beijing)",
                zh: "中国（北京）",
                base: "https://dashscope.aliyuncs.com/compatible-mode/v1",
                discovery: false,
            },
            Service {
                en: "International (Singapore)",
                zh: "国际（新加坡）",
                base: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
                discovery: false,
            },
            Service {
                en: "US (Virginia)",
                zh: "美国（弗吉尼亚）",
                base: "https://dashscope-us.aliyuncs.com/compatible-mode/v1",
                discovery: false,
            },
        ],
    },
    Provider {
        en: "Moonshot / Kimi",
        zh: "月之暗面 / Kimi",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[
            Service {
                en: "China",
                zh: "中国站",
                base: "https://api.moonshot.cn/v1",
                discovery: true,
            },
            Service {
                en: "Global",
                zh: "全球站",
                base: "https://api.moonshot.ai/v1",
                discovery: true,
            },
        ],
    },
    Provider {
        en: "Zhipu / Z.AI / GLM",
        zh: "智谱 / Z.AI / GLM",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[
            Service {
                en: "BigModel (China)",
                zh: "智谱开放平台（中国）",
                base: "https://open.bigmodel.cn/api/paas/v4",
                discovery: false,
            },
            Service {
                en: "Z.AI (Global)",
                zh: "Z.AI（全球）",
                base: "https://api.z.ai/api/paas/v4",
                discovery: false,
            },
        ],
    },
    Provider {
        en: "MiniMax",
        zh: "MiniMax",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[
            Service {
                en: "China",
                zh: "中国站",
                base: "https://api.minimax.cn/v1",
                discovery: false,
            },
            Service {
                en: "Global",
                zh: "全球站",
                base: "https://api.minimax.io/v1",
                discovery: false,
            },
        ],
    },
    Provider {
        en: "Volcengine Ark",
        zh: "火山方舟",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[Service {
            en: "Beijing — standard API",
            zh: "北京 — 标准 API",
            base: "https://ark.cn-beijing.volces.com/api/v3",
            discovery: false,
        }],
    },
    Provider {
        en: "SiliconFlow",
        zh: "硅基流动 / SiliconFlow",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[
            Service {
                en: "China",
                zh: "中国站",
                base: "https://api.siliconflow.cn/v1",
                discovery: true,
            },
            Service {
                en: "Global",
                zh: "全球站",
                base: "https://api.siliconflow.com/v1",
                discovery: true,
            },
        ],
    },
    Provider {
        en: "OpenRouter",
        zh: "OpenRouter",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            base: "https://openrouter.ai/api/v1",
            discovery: true,
        }],
    },
    Provider {
        en: "xAI",
        zh: "xAI",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[
            Service {
                en: "Global",
                zh: "全球入口",
                base: "https://api.x.ai/v1",
                discovery: true,
            },
            Service {
                en: "US regional endpoint",
                zh: "美国区域入口",
                base: "https://us.api.x.ai/v1",
                discovery: true,
            },
        ],
    },
    Provider {
        en: "Mistral",
        zh: "Mistral",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[
            Service {
                en: "Standard endpoint",
                zh: "标准入口",
                base: "https://api.mistral.ai/v1",
                discovery: true,
            },
            Service {
                en: "EU regional endpoint",
                zh: "欧盟区域入口",
                base: "https://api.eu.mistral.ai/v1",
                discovery: true,
            },
            Service {
                en: "US regional endpoint",
                zh: "美国区域入口",
                base: "https://api.us.mistral.ai/v1",
                discovery: true,
            },
        ],
    },
    Provider {
        en: "Groq",
        zh: "Groq",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            base: "https://api.groq.com/openai/v1",
            discovery: true,
        }],
    },
    Provider {
        en: "Together AI",
        zh: "Together AI",
        protocols: &[("openai", "OpenAI Chat Completions")],
        services: &[Service {
            en: "Standard API",
            zh: "标准 API",
            base: "https://api.together.ai/v1",
            discovery: true,
        }],
    },
];

pub(super) fn manual_models(base: &str) -> bool {
    PROVIDERS
        .iter()
        .flat_map(|p| p.services)
        .any(|s| !s.discovery && s.base == base.trim_end_matches('/'))
}
