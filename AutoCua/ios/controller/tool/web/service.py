import importlib
import os
import time
from pathlib import Path

from AutoCua.llm_provider.openrouter.view import get_model_info as get_openrouter_model_info

# Each provider's search (and its SDK) is imported when it is first used:
# importing all of them made the iOS agent's start slow.
_SEARCH_MODULES = {"openrouter": "openrouter", "groq": "groq_search", "openai": "chatgpt",
                   "anthropic": "anthropic", "google": "google_search", "perplexity": "perplexity_search"}


def _web_search(provider: str):
    return importlib.import_module(f".{_SEARCH_MODULES[provider]}", __package__).web_search

# Providers with no native web-search API: the query goes to the browser
# agent on the same provider+model instead (see web_agent.py). Explicit
# membership, not a catch-all - a typo'd provider must still fail fast below.
_BROWSER_FALLBACK_PROVIDERS = frozenset({"together", "cerebras", "aws"})


class WebService:
    """Web service to route queries to appropriate provider"""
    
    def __init__(self, provider: str, model: str, api_key: str = None, vertex: bool = False, vertex_project_id: str = None, vertex_location: str = None, stop_event=None):
        self.provider = provider.lower()
        self.api_key = api_key  # Runtime key from frontend (priority over .env)
        self.stop_event = stop_event  # Only the browser-agent fallback can be interrupted
        # Auto-detect Vertex from model name (e.g. "gemini-3.1-pro-vertex")
        self.vertex = vertex or (self.provider == "google" and model and model.endswith("-vertex"))
        self.vertex_project_id = vertex_project_id
        self.vertex_location = vertex_location
        
        # Resolve short model name to full API name for OpenRouter
        if self.provider == "openrouter":
            model_info = get_openrouter_model_info(model)
            self.model = model_info["api_name"]
        else:
            self.model = model
        
    def search(self, query: str) -> str:
        """Route web search to appropriate provider and format response as JSON object"""
        # Single attempt: a browse run is minutes long, and the browser agent's
        # own LLM manager already retries each call.
        if self.provider in _BROWSER_FALLBACK_PROVIDERS:
            return self._search_via_web_agent(query)

        # Retry up to 3 times with 1 second delay
        for attempt in range(3):
            try:
                if self.provider == "openrouter":
                    result = _web_search("openrouter")(query, self.model, self.api_key)
                elif self.provider == "groq":
                    result = _web_search("groq")(query, self.api_key)  # Groq always uses compound
                elif self.provider == "openai":
                    result = _web_search("openai")(query, self.api_key)  # OpenAI always uses gpt-5.6-terra
                elif self.provider == "anthropic":
                    result = _web_search("anthropic")(query, self.api_key)  # Anthropic uses Haiku 4.5 with native web_search
                elif self.provider == "google":
                    result = _web_search("google")(query, self.api_key, self.vertex, self.vertex_project_id, self.vertex_location)  # Google uses Gemini 3.6 Flash with grounding
                elif self.provider == "perplexity":
                    result = _web_search("perplexity")(query, self.api_key)  # Perplexity uses Sonar with native web search
                else:
                    result = f"Unsupported provider: {self.provider}"
                
                # Format as JSON object (no wrapper tags - agent adds <tool> wrapper)
                formatted_response = f'''{{\ntool: web,\nstatus: success,\nquery: "{query}",\nInformation: "{result}"\n}}'''
                return formatted_response
                
            except Exception as e:
                if attempt < 2:  # If not the last attempt
                    print(f"⚠️ Web search failed (attempt {attempt + 1}/3), retrying in 1 second...")
                    time.sleep(1)
                    continue
                else:
                    # Last attempt failed, return error
                    error_msg = f"Web service error: {str(e)}"
                    return f'''{{\ntool: web,\nstatus: error,\nquery: "{query}",\nNote: Search failed after 3 attempts,\nInformation: "{error_msg}"\n}}'''

    def _search_via_web_agent(self, query: str) -> str:
        """Same result shapes as the native path, produced by the browser agent."""
        from .web_agent import web_search as web_agent_web_search, WebAgentSearchError
        try:
            report = web_agent_web_search(query, self.provider, self.model, self.api_key,
                                          stop_event=self.stop_event)
            return f'''{{\ntool: web,\nstatus: success,\nquery: "{query}",\nInformation: "{report}"\n}}'''
        except WebAgentSearchError as e:
            return f'''{{\ntool: web,\nstatus: error,\nquery: "{query}",\nNote: {e.note},\nInformation: "{e}"\n}}'''
        except Exception as e:
            error_msg = f"Web service error: {str(e)}"
            return f'''{{\ntool: web,\nstatus: error,\nquery: "{query}",\nNote: Browser agent failed to start,\nInformation: "{error_msg}"\n}}'''
