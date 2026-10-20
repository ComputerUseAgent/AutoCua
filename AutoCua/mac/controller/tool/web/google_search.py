import os
from google import genai
from google.genai import types
from dotenv import load_dotenv

load_dotenv()

current_dir = os.path.dirname(os.path.abspath(__file__))
web_md_path = os.path.join(current_dir, "web.md")

with open(web_md_path, "r") as f:
    system_prompt = f.read()


def web_search(query, api_key=None, vertex=False, vertex_project_id=None, vertex_location=None):
    """
    Perform web search using Google Gemini 3.6 Flash with grounding via Google Search

    Args:
        query: Search query
        api_key: Runtime API key from frontend (priority over .env)
        vertex: Whether to use Vertex AI endpoint
        vertex_project_id: GCP project ID (priority over .env)
        vertex_location: GCP region (priority over .env)
    """
    try:
        if vertex:
            project = vertex_project_id or os.getenv('VERTEX_PROJECT_ID')
            location = vertex_location or os.getenv('VERTEX_LOCATION', 'global')
            client = genai.Client(vertexai=True, project=project, location=location)
        else:
            key = api_key or os.getenv('GOOGLE_API_KEY')
            client = genai.Client(api_key=key)

        config = types.GenerateContentConfig(
            system_instruction=system_prompt,
            tools=[types.Tool(google_search=types.GoogleSearch())],
            max_output_tokens=8192,
            seed=42,
        )

        response = client.models.generate_content(
            # Kept in step with llm_provider/google/view.py, which moved off
            # 3.5-flash — otherwise web search and the driver run different
            # Gemini versions.
            model="gemini-3.6-flash",
            contents=query,
            config=config,
        )

        return response.text
    except Exception as e:
        raise Exception(f"Error in web search: {str(e)}")


if __name__ == "__main__":
    query = input("Search: ")
    print(web_search(query))