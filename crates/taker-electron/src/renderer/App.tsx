import logo from "../../assets/logo.svg";
import "./App.css";

export default function App() {
    return (
        <div className="content">
            <img alt="icon" src={logo} className="logo" />
            <p id="message">Starting ItchySats ...</p>
        </div>
    );
}
